//! Live stream: one `snapshot` on connect, then one `event` per change.
//! A subscriber too slow to keep up gets a fresh `snapshot` instead of
//! blocking anyone. A guest follows its rooms only: their devices and the
//! orders held for them, never the journal nor the house's notices.
//!
//! Live streams (this one and the automations') are counted: each holds a
//! connection and a seat on the bus, so a caller holds a few at most, and
//! the house a bounded number ([`Streams`]).

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
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
/// Live streams in all.
const IN_ALL: usize = 64;

/// The live streams open, by caller.
#[derive(Clone, Debug, Default)]
pub(crate) struct Streams(Arc<Mutex<HashMap<String, usize>>>);

/// One open stream's seat, given back when the stream ends.
#[derive(Debug)]
pub(crate) struct Seat {
    streams: Streams,
    key: String,
}

impl Streams {
    /// A seat for one more stream from `key`, if one is left.
    pub(crate) fn take(&self, key: &str) -> Option<Seat> {
        let mut open = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if open.values().sum::<usize>() >= IN_ALL || open.get(key).is_some_and(|n| *n >= PER_CALLER)
        {
            return None;
        }
        *open.entry(key.to_owned()).or_default() += 1;
        Some(Seat {
            streams: self.clone(),
            key: key.to_owned(),
        })
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        let mut open = self
            .streams
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = open.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                open.remove(&self.key);
            }
        }
    }
}

/// No seat left: 503.
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
    // The seat goes with the stream, and comes back when it ends.
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
        .take_until(shutdown.cancelled_owned());
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
            Event::DeviceUpserted { device } => (Some(device.id.clone()), true),
            Event::DeviceRemoved { device } | Event::LabelChanged { device, .. } => {
                (Some(device.clone()), true)
            }
            // A driver's state, a decision on a request: no room in them.
            Event::DriverStatus { .. } | Event::ApprovalResolved { .. } => return Pass::Event,
            // The house's journal and notices are the household's.
            Event::Journal { .. } | Event::Notice { .. } => return Pass::Drop,
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
        let other = streams.take("192.168.0.21");
        assert!(other.is_some(), "another caller still has seats");
        drop(mine);
        assert!(
            streams.take("192.168.0.20").is_some(),
            "a stream that ends gives its seat back"
        );
        let many: Vec<Seat> = (0..IN_ALL)
            .filter_map(|i| streams.take(&format!("fd00::{i:x}")))
            .collect();
        assert_eq!(many.len(), IN_ALL - 1, "the house holds {IN_ALL} at most");
        drop((other, many));
        assert!(streams.0.lock().unwrap().is_empty());
    }
}
