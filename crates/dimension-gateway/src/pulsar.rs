//! Pulsar consumer wrapper used by the run-events SSE handler.
//!
//! One shared client per gateway process; a per-request consumer is created
//! when a client subscribes to a run's event stream. Subscriptions are
//! deleted when the consumer is dropped — Pulsar's exclusive subscription
//! mode would prevent two gateways from reading the same run concurrently,
//! so we use shared subscriptions with a unique random subscription name.
//!
//! Wire format must match `dimension-worker/src/pulsar.rs::PulsarRunEvent`.

use std::sync::Arc;

use pulsar::{Pulsar, SubType as PulsarSubType, TokioExecutor};
use serde::Deserialize;

/// Wire shape (must mirror the worker's `PulsarRunEvent`).
#[derive(Debug, Clone, Deserialize)]
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

#[derive(Clone)]
pub struct PulsarClient {
    pub inner: Arc<Pulsar<TokioExecutor>>,
    pub topic: String,
}

impl PulsarClient {
    pub async fn connect(url: &str, topic: &str) -> Result<Self, String> {
        let client = Pulsar::builder(url, TokioExecutor)
            .build()
            .await
            .map_err(|e| format!("pulsar connect: {e}"))?;
        Ok(Self {
            inner: Arc::new(client),
            topic: topic.to_string(),
        })
    }

    /// Subscribe to the run-events topic in shared mode with a unique name.
    /// Messages are JSON-decoded into `PulsarRunEvent`; the SSE handler is
    /// responsible for filtering by run_id (the partition key).
    pub async fn subscribe_run_events(
        &self,
        sub_suffix: &str,
    ) -> Result<pulsar::Consumer<Vec<u8>, TokioExecutor>, String> {
        let consumer = self
            .inner
            .consumer()
            .with_topic(&self.topic)
            .with_subscription(format!("dimension-gateway-{sub_suffix}"))
            .with_subscription_type(PulsarSubType::Shared)
            .build::<Vec<u8>>()
            .await
            .map_err(|e| format!("pulsar consumer build: {e}"))?;
        Ok(consumer)
    }
}
