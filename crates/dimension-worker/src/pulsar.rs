//! Pulsar producer wrapper for run events.
//!
//! The worker publishes every `RunEvent` to a single topic
//! (default `persistent://dimension/events/runs`) using the `run_id` as the
//! partition key. Per-run order is preserved within a partition; consumers
//! filter by key.
//!
//! When the broker is unreachable, `PulsarPublisher::publish` logs a warning
//! and drops the event — the in-process broadcast and Clickhouse insert
//! paths remain authoritative for delivery.

use std::sync::Arc;

use pulsar::{producer, proto, Pulsar, TokioExecutor};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::events::RunEvent;

/// Wire shape for messages on the Pulsar topic. JSON-encoded.
#[derive(Debug, Clone, Serialize)]
pub struct PulsarRunEvent {
    pub run_id: String,
    pub seq: u64,
    pub ts_ms: i64,
    pub kind: String,
    pub content_type: String,
    pub body: String,
    pub attrs: std::collections::HashMap<String, String>,
    pub worker_id: String,
    pub bundle_id: String,
    pub user_id: String,
}

impl From<&RunEvent> for PulsarRunEvent {
    fn from(ev: &RunEvent) -> Self {
        Self {
            run_id: ev.run_id.to_string(),
            seq: ev.seq,
            ts_ms: ev.ts_ms,
            kind: ev.kind.clone(),
            content_type: ev.content_type.clone(),
            body: String::from_utf8_lossy(&ev.body).into_owned(),
            attrs: ev.attrs.clone(),
            worker_id: ev.worker_id.clone(),
            bundle_id: ev.bundle_id.clone(),
            user_id: ev.user_id.clone(),
        }
    }
}

pub struct PulsarPublisher {
    producer: Mutex<producer::Producer<TokioExecutor>>,
}

impl PulsarPublisher {
    /// Connect to the Pulsar broker and create a producer on `topic`.
    pub async fn connect(url: &str, topic: &str) -> Result<Arc<Self>, String> {
        let client: Pulsar<TokioExecutor> = Pulsar::builder(url, TokioExecutor)
            .build()
            .await
            .map_err(|e| format!("pulsar connect: {e}"))?;

        let producer = client
            .producer()
            .with_topic(topic)
            .with_name("dimension-worker")
            .build()
            .await
            .map_err(|e| format!("pulsar producer build: {e}"))?;

        Ok(Arc::new(Self {
            producer: Mutex::new(producer),
        }))
    }

    /// Fire-and-forget publish. The actual send happens on a background task
    /// so the fan-out loop never blocks on broker latency.
    pub fn publish(self: &Arc<Self>, ev: &RunEvent) {
        let payload = match serde_json::to_vec(&PulsarRunEvent::from(ev)) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "pulsar serialize failed");
                return;
            }
        };
        let key = ev.run_id.to_string();
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut producer = me.producer.lock().await;
            let msg = producer::Message {
                payload,
                partition_key: Some(key),
                ..Default::default()
            };
            if let Err(e) = producer.send_non_blocking(msg).await {
                tracing::warn!(error = %e, "pulsar publish failed");
            }
        });
    }
}

// Make the unused import warnings quiet without polluting the rest of the
// file with `#[allow]` attributes.
#[allow(dead_code)]
fn _unused(_p: proto::MessageMetadata) {}
