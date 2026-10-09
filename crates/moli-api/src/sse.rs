//! Live stream: one `snapshot` on connect, then one `event` per change.
//! A subscriber too slow to keep up gets a fresh `snapshot` instead of
//! blocking anyone.

use std::convert::Infallible;

use axum::Extension;
use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures::Stream;
use futures::stream::{self, StreamExt};
use moli_runtime::Hub;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

pub(crate) async fn events(
    State(hub): State<Hub>,
    Extension(shutdown): Extension<CancellationToken>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    // Subscribe before the snapshot: nothing can fall between the two.
    let rx = hub.subscribe();
    let first = snapshot(&hub);
    let rest = stream::unfold((hub, rx), |(hub, mut rx)| async move {
        let event = match rx.recv().await {
            Ok(event) => SseEvent::default().event("event").json_data(&event).ok()?,
            Err(RecvError::Lagged(missed)) => {
                tracing::debug!(missed, "sse subscriber lagged, resending snapshot");
                snapshot(&hub)
            }
            Err(RecvError::Closed) => return None,
        };
        Some((Ok(event), (hub, rx)))
    });
    let stream = stream::once(async { Ok(first) })
        .chain(rest)
        .take_until(shutdown.cancelled_owned());
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn snapshot(hub: &Hub) -> SseEvent {
    SseEvent::default()
        .event("snapshot")
        .json_data(hub.snapshot())
        .unwrap_or_else(|_| SseEvent::default().event("error"))
}
