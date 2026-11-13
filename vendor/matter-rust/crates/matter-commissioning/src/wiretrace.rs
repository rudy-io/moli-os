//! JSON-lines capture of `matter_wire` trace events (M6 cross-verification).
//!
//! [`JsonlLayer`] is a [`tracing_subscriber::Layer`] that listens for the
//! structured wire-trace events the `driver` emits on the `matter_wire`
//! target (one per decrypted message in either direction) and writes one
//! JSON object per line:
//!
//! ```json
//! {"seq":3,"dir":"tx","session_id":0,"exchange":1,"protocol":0,"opcode":32,"payload":"15..."}
//! ```
//!
//! `cargo xtask trace-diff` consumes two such files (ours and matter.js's)
//! and compares the dialogues structurally. This is tooling-grade
//! infrastructure: write errors are silently dropped rather than
//! propagated, because a trace capture must never abort a live
//! commissioning run.
//!
//! # Debugging only: never enable `wiretrace` in production
//!
//! The `payload` of each event is the **full decrypted plaintext** of the
//! message. During commissioning that includes the IPK (`AddNOC`), the
//! Wi-Fi passphrase (`AddOrUpdateWiFiNetwork`) and the Thread operational
//! dataset, network key included (`AddOrUpdateThreadNetwork`). With
//! `wiretrace` on, every subscriber that accepts `matter_wire` debug events
//! sees those bytes, not just this layer: a `fmt` layer at `debug` level
//! prints them too. Under plain `tracing` (without `wiretrace`) the events
//! carry no `payload` field at all.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// The `tracing` target wire-trace events are emitted on.
pub const WIRE_TARGET: &str = "matter_wire";

/// A [`Layer`] that serializes `matter_wire` events as JSON lines to `W`.
///
/// Install it alongside a normal `fmt` layer:
///
/// ```ignore
/// let file = std::fs::File::create("trace.jsonl")?;
/// let registry = tracing_subscriber::registry()
///     .with(fmt_layer)
///     .with(JsonlLayer::new(file));
/// ```
pub struct JsonlLayer<W: Write + Send + 'static> {
    seq: AtomicU64,
    writer: Mutex<W>,
}

impl<W: Write + Send + 'static> JsonlLayer<W> {
    /// Wrap `writer`; each captured event becomes one JSON line.
    pub fn new(writer: W) -> Self {
        Self {
            seq: AtomicU64::new(0),
            writer: Mutex::new(writer),
        }
    }
}

/// Field extractor for one `matter_wire` event.
#[derive(Default)]
struct WireVisitor {
    dir: Option<String>,
    session_id: Option<u64>,
    exchange_id: Option<u64>,
    protocol: Option<u64>,
    opcode: Option<u64>,
    payload: Option<String>,
}

impl Visit for WireVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "session_id" => self.session_id = Some(value),
            "exchange_id" => self.exchange_id = Some(value),
            "protocol" => self.protocol = Some(value),
            "opcode" => self.opcode = Some(value),
            _ => {}
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if let Ok(v) = u64::try_from(value) {
            self.record_u64(field, v);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "dir" => self.dir = Some(value.to_owned()),
            "payload" => self.payload = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // `%`-recorded values arrive here as tracing's Display wrapper,
        // whose Debug forwards to Display — so this yields the raw string
        // with no quotes. The trim is belt-and-suspenders for a future
        // plainly-Debug-recorded &str; hex payloads contain no quotes, so
        // it can never corrupt real data.
        if matches!(field.name(), "dir" | "payload") {
            let rendered = format!("{value:?}");
            let trimmed = rendered.trim_matches('"').to_owned();
            match field.name() {
                "dir" => self.dir = Some(trimmed),
                "payload" => self.payload = Some(trimmed),
                _ => {}
            }
        }
    }
}

impl<S, W> Layer<S> for JsonlLayer<W>
where
    S: tracing::Subscriber,
    W: Write + Send + 'static,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != WIRE_TARGET {
            return;
        }
        let mut v = WireVisitor::default();
        event.record(&mut v);
        let (
            Some(dir),
            Some(session_id),
            Some(exchange),
            Some(protocol),
            Some(opcode),
            Some(payload),
        ) = (
            v.dir,
            v.session_id,
            v.exchange_id,
            v.protocol,
            v.opcode,
            v.payload,
        )
        else {
            // A matter_wire event missing schema fields is a library bug,
            // but a capture layer must not panic mid-commission; drop it.
            return;
        };
        if let Ok(mut w) = self.writer.lock() {
            // Assign seq and write under the same lock so file order matches
            // seq order unconditionally, even when concurrent events race.
            let seq = self.seq.fetch_add(1, Ordering::Relaxed);
            let line = serde_json::json!({
                "seq": seq,
                "dir": dir,
                "session_id": session_id,
                "exchange": exchange,
                "protocol": protocol,
                "opcode": opcode,
                "payload": payload,
            });
            // Tooling-grade: ignore write errors (see module docs).
            let _ = writeln!(w, "{line}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    /// `Write` adapter over a shared buffer so the test can read what the
    /// layer wrote after the subscriber guard drops.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn layer_serializes_wire_events_and_ignores_others() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let layer = JsonlLayer::new(SharedBuf(buf.clone()));
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(
                target: "matter_wire",
                dir = "tx",
                session_id = 0_u64,
                exchange_id = 1_u64,
                protocol = 0_u64,
                opcode = 0x20_u64,
                payload = %"15300120aa18",
                "wire"
            );
            // Non-wire events must not produce lines.
            tracing::debug!(unrelated = 1, "noise");
            tracing::debug!(
                target: "matter_wire",
                dir = "rx",
                session_id = 0_u64,
                exchange_id = 1_u64,
                protocol = 0_u64,
                opcode = 0x21_u64,
                payload = %"153001",
                "wire"
            );
        });
        let bytes = buf.lock().unwrap().clone();
        let text = String::from_utf8(bytes).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["seq"], 0);
        assert_eq!(lines[0]["dir"], "tx");
        assert_eq!(lines[0]["session_id"], 0);
        assert_eq!(lines[0]["exchange"], 1);
        assert_eq!(lines[0]["protocol"], 0);
        assert_eq!(lines[0]["opcode"], 0x20);
        assert_eq!(lines[0]["payload"], "15300120aa18");
        assert_eq!(lines[1]["seq"], 1);
        assert_eq!(lines[1]["dir"], "rx");
    }

    #[test]
    fn wire_event_missing_schema_field_is_dropped() {
        // A `matter_wire` event that omits `opcode` must produce zero lines.
        let buf = Arc::new(Mutex::new(Vec::new()));
        let layer = JsonlLayer::new(SharedBuf(buf.clone()));
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(
                target: "matter_wire",
                dir = "tx",
                session_id = 0_u64,
                exchange_id = 1_u64,
                protocol = 0_u64,
                // `opcode` intentionally omitted
                payload = %"deadbeef",
                "wire"
            );
        });
        let bytes = buf.lock().unwrap().clone();
        assert!(bytes.is_empty(), "expected no output for incomplete event");
    }

    #[test]
    fn empty_str_payload_serializes_as_empty_string() {
        // A standalone-ack has an empty payload. The `record_str` path (used
        // when the field is not `%`-recorded) must store "" as-is.
        let buf = Arc::new(Mutex::new(Vec::new()));
        let layer = JsonlLayer::new(SharedBuf(buf.clone()));
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(
                target: "matter_wire",
                dir = "tx",
                session_id = 0_u64,
                exchange_id = 1_u64,
                protocol = 0_u64,
                opcode = 0x10_u64,
                payload = "",   // exercises record_str with empty string
                "wire"
            );
        });
        let bytes = buf.lock().unwrap().clone();
        let text = String::from_utf8(bytes).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 1, "expected exactly one line");
        assert_eq!(lines[0]["payload"], "", "payload must be empty string");
    }
}
