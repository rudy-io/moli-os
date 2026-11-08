//! Live stream: one `snapshot` on connect, then one `event` per change.
//! A subscriber too slow to keep up gets a fresh `snapshot` instead of
//! blocking anyone. A guest follows its rooms only: their devices and the
//! orders held for them, never the journal nor the house's notices.
//!
//! Live streams (this one and the automations') are counted: each holds a
//! connection and a seat on the bus, so a caller holds a few at most, and
//! the house a bounded number ([`Streams`]). The pool never refuses a whole
//! house: when it is full, the oldest stream is evicted, so a client whose
//! address keeps rotating cannot starve the family's live views.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures::stream::{self, StreamExt};
use moli_core::{DeviceId, Event};
use moli_runtime::Hub;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use crate::caller::Caller;
use crate::people::PersonRef;

/// Live streams one caller may hold at once (a few tabs, a wall tablet).
const PER_CALLER: usize = 8;
/// Live streams in all. When the pool is full, the oldest stream is evicted
/// instead of refusing: a caller whose address keeps rotating (IPv6 privacy
/// extensions) must not be able to starve the house's live views.
const IN_ALL: usize = 64;

/// The live streams open: per caller, the seat ids (oldest first), and each
/// seat's cancellation token (an eviction ends the stream).
#[derive(Debug, Default)]
struct Table {
    open: HashMap<String, Vec<u64>>,
    tokens: HashMap<u64, CancellationToken>,
    next: u64,
}

/// The live streams open, by caller.
#[derive(Clone, Debug, Default)]
pub(crate) struct Streams(Arc<Mutex<Table>>);

/// One open stream's seat, given back when the stream ends.
#[derive(Debug)]
pub(crate) struct Seat {
    streams: Streams,
    key: String,
    id: u64,
    pub(crate) token: CancellationToken,
}

impl Streams {
    /// A seat for one more stream from `key`. Refused only when the caller
    /// already holds [`PER_CALLER`]; when the pool is full, the oldest
    /// stream is evicted to make room.
    pub(crate) fn take(&self, key: &str) -> Option<Seat> {
        let mut t = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if t.open
            .get(key)
            .is_some_and(|seats| seats.len() >= PER_CALLER)
        {
            return None;
        }
        if t.open.values().map(Vec::len).sum::<usize>() >= IN_ALL {
            // The oldest seat, among all callers: it makes room.
            let oldest = t
                .open
                .iter()
                .filter_map(|(caller, seats)| {
                    seats
                        .first()
                        .copied()
                        .map(|seat_id| (caller.clone(), seat_id))
                })
                .min_by_key(|(_, seat_id)| *seat_id);
            if let Some((caller, seat_id)) = oldest {
                let seats = t
                    .open
                    .get_mut(&caller)
                    .expect("caller just read from the table");
                seats.remove(0);
                if seats.is_empty() {
                    t.open.remove(&caller);
                }
                if let Some(token) = t.tokens.remove(&seat_id) {
                    tracing::info!(client = %caller, "live stream pool full: oldest stream evicted");
                    token.cancel();
                }
            }
        }
        let seat_id = t.next;
        t.next += 1;
        let token = CancellationToken::new();
        t.tokens.insert(seat_id, token.clone());
        t.open.entry(key.to_owned()).or_default().push(seat_id);
        Some(Seat {
            streams: self.clone(),
            key: key.to_owned(),
            id: seat_id,
            token,
        })
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        let mut t = self
            .streams
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Nothing to give back when this seat was already evicted.
        if let Some(seats) = t.open.get_mut(&self.key) {
            if let Some(i) = seats.iter().position(|s| *s == self.id) {
                seats.remove(i);
            }
            if seats.is_empty() {
                t.open.remove(&self.key);
            }
        }
        t.tokens.remove(&self.id);
        self.token.cancel();
    }
}

/// Ends when the house shuts down or when the pool evicts this seat.
pub(crate) fn until_end(
    shutdown: &CancellationToken,
    seat: &CancellationToken,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    let shutdown = shutdown.clone();
    let seat = seat.clone();
    Box::pin(async move {
        tokio::select! {
            () = shutdown.cancelled_owned() => {}
            () = seat.cancelled_owned() => {}
        }
    })
}

/// The caller holds too many streams of its own: 503.
pub(crate) fn full(key: &str) -> Response {
    tracing::warn!(client = key, "too many live streams");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": moli_i18n::tr!("serveur.limite.trop_de_flux") })),
    )
        .into_response()
}

pub(crate) async fn events(
    State(hub): State<Hub>,
    Extension(shutdown): Extension<CancellationToken>,
    Extension(streams): Extension<Streams>,
    caller: Caller,
) -> Response {
    let Some(seat) = streams.take(&caller.key) else {
        return full(&caller.key);
    };
    let mut view = View {
        asker: caller.asker(),
        guest: caller.person.filter(PersonRef::is_guest),
        sight: HashSet::new(),
    };
    // Subscribe before the snapshot: nothing can fall between the two.
    let rx = hub.subscribe();
    let first = view.snapshot(&hub);
    // The seat goes with the stream, and comes back when it ends; when the
    // pool evicts the seat, its token ends the stream too.
    let seat_token = seat.token.clone();
    let state = (hub, rx, view, seat);
    let rest = stream::unfold(state, |(hub, mut rx, mut view, seat)| async move {
        let event = loop {
            match rx.recv().await {
                Ok(event) => match view.pass(&hub, &event) {
                    Pass::Event => {
                        break SseEvent::default().event("event").json_data(&event).ok()?;
                    }
                    Pass::Snapshot => break view.snapshot(&hub),
                    Pass::Drop => {}
                },
                Err(RecvError::Lagged(missed)) => {
                    tracing::debug!(missed, "sse subscriber lagged, resending snapshot");
                    break view.snapshot(&hub);
                }
                Err(RecvError::Closed) => return None,
            }
        };
        Some((Ok::<_, Infallible>(event), (hub, rx, view, seat)))
    });
    let stream = stream::once(async { Ok(first) })
        .chain(rest)
        .take_until(until_end(&shutdown, &seat_token));
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// What one subscriber may see: the whole house, or a guest's rooms.
struct View {
    /// Who this subscriber is (`Caller::asker`): an answer meant for
    /// someone else is not shown here.
    asker: String,
    guest: Option<PersonRef>,
    /// The devices the guest saw in its last snapshot.
    sight: HashSet<DeviceId>,
}

enum Pass {
    Event,
    /// What the guest sees changed: a fresh snapshot instead.
    Snapshot,
    Drop,
}

impl View {
    fn snapshot(&mut self, hub: &Hub) -> SseEvent {
        let snapshot = match &self.guest {
            Some(guest) => {
                let (snapshot, sight) = guest.guest_view(hub);
                self.sight = sight;
                snapshot
            }
            None => hub.snapshot(),
        };
        SseEvent::default()
            .event("snapshot")
            .json_data(snapshot)
            .unwrap_or_else(|_| SseEvent::default().event("error"))
    }

    /// Whether `event` goes to this subscriber.
    fn pass(&self, hub: &Hub, event: &Event) -> Pass {
        if let Event::Notice { notice } = event
            && notice.to.as_ref().is_some_and(|to| *to != self.asker)
        {
            return Pass::Drop;
        }
        let Some(guest) = &self.guest else {
            return Pass::Event;
        };
        let (device, moved) = match event {
            Event::State { point, .. } => (point.split().map(|(id, _)| id), false),
            Event::Availability { device, .. } => (Some(device.clone()), false),
            Event::ApprovalRequested { request } => {
                (request.point.split().map(|(id, _)| id), false)
            }
            Event::DeviceUpserted { device } | Event::DeviceRestored { device, .. } => {
                (Some(device.id.clone()), true)
            }
            Event::DeviceRemoved { device } | Event::LabelChanged { device, .. } => {
                (Some(device.clone()), true)
            }
            // A driver's state, a driver's registration, a decision on a
            // request, a layout change, the house's journal and notices:
            // none of it is part of a guest's view.
            Event::DriverStatus { .. }
            | Event::DriverRegistered { .. }
            | Event::ApprovalResolved { .. }
            | Event::LayoutChanged {}
            | Event::Journal { .. }
            | Event::Notice { .. } => return Pass::Drop,
        };
        // Described anew, moved, gone: what the guest sees may have changed.
        if moved && guest.guest_devices(hub, &hub.snapshot()) != self.sight {
            return Pass::Snapshot;
        }
        match device {
            Some(id) if self.sight.contains(&id) => Pass::Event,
            _ => Pass::Drop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guest never receives decisions on approvals nor driver states:
    /// they carry the house's activity outside its rooms (journal/notices
    /// territory, which guests must not see at all).
    #[test]
    fn a_guest_never_sees_decisions_nor_driver_states() {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let guest = PersonRef {
            id: "guest".into(),
            name: "Invité".into(),
            role: crate::people::Role::Guest,
            rooms: vec!["Salon".into()],
        };
        let view = View {
            asker: "192.168.0.50".into(),
            guest: Some(guest),
            sight: HashSet::new(),
        };
        let resolved = Event::ApprovalResolved {
            id: 1,
            decision: moli_core::Decision::Approved,
        };
        let status = Event::DriverStatus {
            instance: moli_core::InstanceId::from("fake"),
            status: moli_core::DriverStatus::Running,
            warning: None,
        };
        assert!(matches!(view.pass(&hub, &resolved), Pass::Drop));
        assert!(matches!(view.pass(&hub, &status), Pass::Drop));
        // The household still sees everything.
        let open = View {
            asker: "192.168.0.50".into(),
            guest: None,
            sight: HashSet::new(),
        };
        assert!(matches!(open.pass(&hub, &resolved), Pass::Event));
        assert!(matches!(open.pass(&hub, &status), Pass::Event));
    }

    #[test]
    fn an_answer_for_one_person_is_shown_to_them_only() {
        let hub = Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let view = |asker: &str| View {
            asker: asker.into(),
            guest: None,
            sight: HashSet::new(),
        };
        let notice = |to: Option<&str>| Event::Notice {
            notice: moli_core::Notice {
                title: None,
                message: "Le vol part à 7 h.".into(),
                from: None,
                ts: 0,
                to: to.map(str::to_owned),
            },
        };
        let claire = Some("person:claire");
        assert!(matches!(
            view("person:claire").pass(&hub, &notice(claire)),
            Pass::Event
        ));
        assert!(matches!(
            view("person:thomas").pass(&hub, &notice(claire)),
            Pass::Drop
        ));
        // For the whole house, as before.
        assert!(matches!(
            view("person:thomas").pass(&hub, &notice(None)),
            Pass::Event
        ));
    }

    #[test]
    fn streams_are_counted_per_caller_and_in_all() {
        let streams = Streams::default();
        let mine: Vec<Seat> = (0..PER_CALLER)
            .map(|_| streams.take("192.168.0.20").unwrap())
            .collect();
        assert!(streams.take("192.168.0.20").is_none(), "one too many");
        assert!(
            streams.take("192.168.0.21").is_some(),
            "another caller is unaffected"
        );
        drop(mine);
        assert!(
            streams.take("192.168.0.20").is_some(),
            "a stream that ends gives its seat back"
        );
    }

    #[test]
    fn a_full_pool_evicts_the_oldest_stream() {
        // A caller whose address keeps rotating (one key per address) must
        // not be able to fill the house and starve the live views: the pool
        // evicts its oldest stream instead of refusing.
        let streams = Streams::default();
        let oldest = streams.take("fd00::1").unwrap();
        let rest: Vec<Seat> = (1..IN_ALL)
            .map(|i| streams.take(&format!("fd00::{i}")).unwrap())
            .collect();
        assert_eq!(
            open_count(&streams),
            IN_ALL,
            "the house holds {IN_ALL} at most"
        );
        let newest = streams.take("fd00::200").unwrap();
        assert_eq!(
            open_count(&streams),
            IN_ALL,
            "a new stream displaces an old one"
        );
        assert!(oldest.token.is_cancelled(), "the oldest stream is evicted");
        assert!(
            !newest.token.is_cancelled(),
            "the new stream is not evicted"
        );
        // Dropping an already-evicted seat must not corrupt the table.
        drop((oldest, rest, newest));
        let t = streams.0.lock().unwrap();
        assert!(t.open.is_empty() && t.tokens.is_empty());
    }

    fn open_count(streams: &Streams) -> usize {
        streams.0.lock().unwrap().open.values().map(Vec::len).sum()
    }
}
