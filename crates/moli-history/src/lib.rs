//! History: every value change, kept in SQLite.
//!
//! A recorder subscribes to the hub's bus (which only carries changes) and
//! hands batches to a writer task, so the bus is always read promptly; the
//! retention pass runs on its own. Reads come in two shapes: series for
//! charts (raw, or bucketed when long) and — the agent-native part —
//! read-only SQL over a simple view, so any agent can ask its own questions
//! (« combien de fois la porte s'est ouverte cette semaine ? »).

mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use moli_core::{Event, PointId, Value, now_ms};
use moli_runtime::Hub;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use store::{Bucket, HistoryError, Series, SqlResult, Store};

const BATCH_EVERY: Duration = Duration::from_secs(2);
const BATCH_MAX: usize = 1_000;
const WRITER_QUEUE: usize = 64;
const RETENTION_EVERY: Duration = Duration::from_secs(6 * 3600);

type Row = (PointId, Value, u64);

/// A batch for the writer, and whether it re-states known values.
type Batch = (Vec<Row>, bool);

#[derive(Clone, Debug)]
pub struct Options {
    pub path: PathBuf,
    pub retention_days: u32,
}

/// Cheap handle used by the surfaces to read history.
#[derive(Clone, Debug)]
pub struct History {
    store: Arc<Store>,
}

impl History {
    /// Opens the database and starts recording. Start it *before* the
    /// drivers: it subscribes first, then records the current state, so no
    /// value is missed. The handle resolves once everything is flushed.
    pub fn start(
        hub: &Hub,
        options: &Options,
        cancel: CancellationToken,
    ) -> Result<(Self, JoinHandle<()>), HistoryError> {
        let store = Arc::new(Store::open(&options.path)?);
        let (batches, queue) = mpsc::channel::<Batch>(WRITER_QUEUE);
        let writer = tokio::spawn(write_loop(Arc::clone(&store), queue));
        tokio::spawn(prune_loop(
            Arc::clone(&store),
            options.retention_days,
            cancel.clone(),
        ));
        let source = hub_events(hub);
        let recorder = tokio::spawn(async move {
            record(source, batches, cancel).await;
            let _ = writer.await;
        });
        Ok((Self { store }, recorder))
    }

    #[must_use]
    pub fn from_store(store: Arc<Store>) -> Self {
        Self { store }
    }

    pub async fn series(
        &self,
        point: &str,
        from_ms: u64,
        to_ms: u64,
        max_points: usize,
    ) -> Result<Series, HistoryError> {
        let (store, point) = (Arc::clone(&self.store), point.to_owned());
        blocking(move || store.series(&point, from_ms, to_ms, max_points)).await
    }

    pub async fn sql(&self, sql: &str, max_rows: usize) -> Result<SqlResult, HistoryError> {
        let (store, sql) = (Arc::clone(&self.store), sql.to_owned());
        blocking(move || store.query_sql(&sql, max_rows)).await
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, HistoryError> + Send + 'static,
) -> Result<T, HistoryError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| HistoryError::Internal(e.to_string()))?
}

/// What the recorder listens to: the hub's bus, and its current state
/// (re-read at start and after any lag).
struct HubEvents {
    hub: Hub,
    events: tokio::sync::broadcast::Receiver<Event>,
}

fn hub_events(hub: &Hub) -> HubEvents {
    // Subscribe before reading the state: nothing falls between the two.
    let events = hub.subscribe();
    HubEvents {
        hub: hub.clone(),
        events,
    }
}

fn current_state(hub: &Hub) -> Vec<Row> {
    hub.snapshot()
        .devices
        .iter()
        .flat_map(|d| {
            d.state.iter().map(|(key, sample)| {
                (
                    PointId::new(&d.device.id, key),
                    sample.value.clone(),
                    sample.ts,
                )
            })
        })
        .collect()
}

async fn record(mut source: HubEvents, batches: mpsc::Sender<Batch>, cancel: CancellationToken) {
    let _ = batches.send((current_state(&source.hub), true)).await;
    let mut batch = Vec::new();
    let mut flush = tokio::time::interval(BATCH_EVERY);
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            event = source.events.recv() => match event {
                Ok(Event::State { point, value, ts }) => {
                    batch.push((point, value, ts));
                    if batch.len() >= BATCH_MAX {
                        hand_over(&batches, &mut batch);
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(missed)) => {
                    // Changes were missed; at least the present is recorded.
                    tracing::warn!(missed, "history recorder lagged, re-recording the current state");
                    hand_over(&batches, &mut batch);
                    let _ = batches.try_send((current_state(&source.hub), true));
                }
                Err(RecvError::Closed) => break,
            },
            _ = flush.tick() => hand_over(&batches, &mut batch),
        }
    }
    if !batch.is_empty() {
        let _ = batches.send((batch, false)).await;
    }
}

/// Never blocks the bus: a full writer queue keeps the batch for later.
fn hand_over(batches: &mpsc::Sender<Batch>, batch: &mut Vec<Row>) {
    if batch.is_empty() {
        return;
    }
    match batches.try_send((std::mem::take(batch), false)) {
        Ok(()) => {}
        Err(
            mpsc::error::TrySendError::Full((rows, _))
            | mpsc::error::TrySendError::Closed((rows, _)),
        ) => {
            tracing::warn!(rows = rows.len(), "history writer busy, keeping the batch");
            *batch = rows;
        }
    }
}

async fn write_loop(store: Arc<Store>, mut queue: mpsc::Receiver<Batch>) {
    while let Some((rows, restate)) = queue.recv().await {
        let count = rows.len();
        let store = Arc::clone(&store);
        if let Err(e) = blocking(move || store.insert(&rows, restate)).await {
            tracing::warn!(error = %e, count, "history batch not written");
        }
    }
}

async fn prune_loop(store: Arc<Store>, retention_days: u32, cancel: CancellationToken) {
    let mut tick = tokio::time::interval(RETENTION_EVERY);
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = tick.tick() => {}
        }
        let cutoff = now_ms().saturating_sub(u64::from(retention_days) * 86_400_000);
        let store = Arc::clone(&store);
        match blocking(move || store.prune(cutoff)).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(removed = n, "history pruned"),
            Err(e) => tracing::warn!(error = %e, "history pruning failed"),
        }
    }
}
