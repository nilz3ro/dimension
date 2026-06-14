//! In-memory log broadcast channel for live log streaming.
//!
//! [`LogBroadcaster`] wraps a `tokio::sync::broadcast` channel and a ring
//! buffer of recent lines. It is cloned into [`AppState`] so that the
//! `GET /admin/logs/tail` SSE handler can subscribe to the live stream and
//! immediately replay recent history to new subscribers.
//!
//! [`BroadcastLayer`] is a [`tracing_subscriber::Layer`] that intercepts every
//! tracing event and forwards a JSON-formatted line to the broadcaster.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Maximum number of recent log lines kept in the ring buffer.
const LOG_BUFFER_SIZE: usize = 1000;

/// Capacity of the broadcast channel (messages in flight before lagging).
const LOG_CHANNEL_CAPACITY: usize = 256;

// ── LogBroadcaster ───────────────────────────────────────────────────────────

/// In-memory log broadcaster.
///
/// Cloneable handle to a shared broadcast channel + ring buffer. All clones
/// point to the same underlying state; there is only one broadcaster per
/// gateway process.
#[derive(Clone)]
pub struct LogBroadcaster {
    tx: broadcast::Sender<String>,
    buffer: Arc<Mutex<VecDeque<String>>>,
}

impl LogBroadcaster {
    /// Create a new broadcaster with an empty ring buffer.
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(LOG_CHANNEL_CAPACITY);
        Self {
            tx,
            buffer: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_BUFFER_SIZE))),
        }
    }

    /// Subscribe to the live broadcast stream.
    ///
    /// The returned [`broadcast::Receiver`] will receive all future log lines
    /// published via [`send`](Self::send). Lines published before the call are
    /// only available via [`recent`](Self::recent).
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// Return a snapshot of the recent log buffer (up to 1000 lines).
    ///
    /// New SSE subscribers call this to replay history before switching to the
    /// live stream, so they do not miss lines produced before they connected.
    pub fn recent(&self) -> Vec<String> {
        self.buffer.lock().unwrap().iter().cloned().collect()
    }

    /// Publish a log line to the ring buffer and the broadcast channel.
    ///
    /// If the ring buffer is full, the oldest line is evicted. Send errors
    /// (no active receivers) are silently ignored — it is fine to log without
    /// any SSE subscribers connected.
    pub fn send(&self, line: String) {
        {
            let mut buf = self.buffer.lock().unwrap();
            if buf.len() >= LOG_BUFFER_SIZE {
                buf.pop_front();
            }
            buf.push_back(line.clone());
        }
        // Ignore send errors (no active subscribers is fine).
        let _ = self.tx.send(line);
    }
}

impl Default for LogBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

// ── BroadcastLayer ───────────────────────────────────────────────────────────

/// Tracing subscriber [`Layer`] that forwards every log event to a
/// [`LogBroadcaster`] as a single-line JSON string.
pub struct BroadcastLayer {
    broadcaster: LogBroadcaster,
}

impl BroadcastLayer {
    /// Create a new layer that sends events to `broadcaster`.
    pub fn new(broadcaster: LogBroadcaster) -> Self {
        Self { broadcaster }
    }
}

impl<S: Subscriber> Layer<S> for BroadcastLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = event.metadata().level().to_string();
        let target = event.metadata().target();

        let mut fields: HashMap<String, serde_json::Value> = HashMap::new();
        let mut visitor = FieldVisitor(&mut fields);
        event.record(&mut visitor);

        let line = serde_json::json!({
            "level": level,
            "target": target,
            "fields": fields,
        })
        .to_string();

        self.broadcaster.send(line);
    }
}

// ── FieldVisitor ─────────────────────────────────────────────────────────────

/// Collects tracing event fields into a `HashMap<String, serde_json::Value>`.
struct FieldVisitor<'a>(&'a mut HashMap<String, serde_json::Value>);

impl tracing::field::Visit for FieldVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0
            .insert(field.name().to_string(), serde_json::Value::String(value.to_string()));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(
            field.name().to_string(),
            serde_json::Value::String(format!("{:?}", value)),
        );
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.0.insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.0.insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0.insert(field.name().to_string(), serde_json::json!(value));
    }
}
