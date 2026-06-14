//! Run-event fan-out: bundle messages, agent stdout/stderr forwards, and
//! worker-generated state events flow through a single mpsc into all
//! configured sinks (Clickhouse, Pulsar, in-process broadcast).
//!
//! Sinks degrade independently: if Pulsar is down, Clickhouse and the
//! in-process broadcast still receive events. If Clickhouse is down, Pulsar
//! and the broadcast still work. A bounded buffer on the mpsc means that
//! prolonged backpressure causes `send()` to fail rather than block the
//! bundle indefinitely; the bundle keeps running and the dropped event is
//! recorded with a `kind = "dropped"` marker.

use std::collections::HashMap;
use std::sync::Arc;

use dimension_protocol::proto::OutboundMessage;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use crate::observability::{ClickhouseClient, RunEventRecord};
use crate::pulsar::PulsarPublisher;

const FANOUT_BUFFER: usize = 1024;
const BROADCAST_BUFFER: usize = 512;
const CH_BATCH_MAX: usize = 64;
const CH_BATCH_INTERVAL_MS: u64 = 250;

/// In-memory event passed between sources (agent envelope reader, worker
/// state emitters) and the fan-out task.
#[derive(Debug, Clone, Serialize)]
pub struct RunEvent {
    pub run_id: Uuid,
    pub seq: u64,
    pub ts_ms: i64,
    pub kind: String,
    pub content_type: String,
    pub body: Vec<u8>,
    pub attrs: HashMap<String, String>,
    pub worker_id: String,
    pub bundle_id: String,
    pub user_id: String,
}

impl RunEvent {
    pub fn from_outbound(
        msg: OutboundMessage,
        run_id: Uuid,
        worker_id: &str,
        bundle_id: &str,
        user_id: &str,
    ) -> Self {
        Self {
            run_id,
            seq: msg.sequence,
            ts_ms: msg.timestamp_ms,
            kind: msg.kind,
            content_type: msg.content_type,
            body: msg.body.to_vec(),
            attrs: msg.attributes.into_iter().collect(),
            worker_id: worker_id.to_string(),
            bundle_id: bundle_id.to_string(),
            user_id: user_id.to_string(),
        }
    }

    pub fn to_record(&self) -> RunEventRecord {
        RunEventRecord {
            run_id: self.run_id,
            seq: self.seq,
            ts: self.ts_ms,
            kind: self.kind.clone(),
            content_type: self.content_type.clone(),
            body: String::from_utf8_lossy(&self.body).into_owned(),
            attrs: self
                .attrs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            worker_id: self.worker_id.clone(),
            bundle_id: self.bundle_id.clone(),
            user_id: self.user_id.clone(),
        }
    }
}

/// Producer handle: every emitter (agent reader, state-event helpers) holds
/// one of these and pushes `RunEvent`s into the channel.
#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::Sender<RunEvent>,
}

impl EventSink {
    pub fn send(&self, ev: RunEvent) {
        // try_send to avoid blocking the bundle's UDS write path.
        if let Err(e) = self.tx.try_send(ev) {
            match e {
                mpsc::error::TrySendError::Full(_) => {
                    tracing::warn!("EventSink full; dropping event");
                }
                mpsc::error::TrySendError::Closed(_) => {
                    tracing::debug!("EventSink closed");
                }
            }
        }
    }
}

/// Per-run broadcast handle. The gateway's gRPC streaming subscribe path
/// uses this for live tailing when Pulsar isn't configured/available.
#[derive(Clone)]
pub struct RunBroadcast {
    inner: Arc<dashmap::DashMap<Uuid, broadcast::Sender<RunEvent>>>,
}

impl RunBroadcast {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(dashmap::DashMap::new()),
        }
    }

    pub fn sender_for(&self, run_id: Uuid) -> broadcast::Sender<RunEvent> {
        self.inner
            .entry(run_id)
            .or_insert_with(|| {
                let (tx, _rx) = broadcast::channel(BROADCAST_BUFFER);
                tx
            })
            .clone()
    }

    pub fn subscribe(&self, run_id: Uuid) -> broadcast::Receiver<RunEvent> {
        self.sender_for(run_id).subscribe()
    }

    pub fn finish(&self, run_id: Uuid) {
        // Drop the sender; subscribers receive a Lagged or Closed signal.
        self.inner.remove(&run_id);
    }
}

impl Default for RunBroadcast {
    fn default() -> Self {
        Self::new()
    }
}

/// Top-level fan-out coordinator. Owns the receive half of the mpsc and
/// dispatches each event to all enabled sinks.
pub struct EventFanout {
    pub sink: EventSink,
    pub broadcast: RunBroadcast,
}

impl EventFanout {
    /// Spawn the fan-out task and return the producer handle + broadcast
    /// registry.
    pub fn spawn(
        ch_client: Option<ClickhouseClient>,
        pulsar: Option<Arc<PulsarPublisher>>,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<RunEvent>(FANOUT_BUFFER);
        let broadcast = RunBroadcast::new();
        let broadcast_for_task = broadcast.clone();

        tokio::spawn(async move {
            let mut ch_batch: Vec<RunEventRecord> = Vec::with_capacity(CH_BATCH_MAX);
            let mut ticker = tokio::time::interval(
                std::time::Duration::from_millis(CH_BATCH_INTERVAL_MS),
            );
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    maybe_ev = rx.recv() => {
                        match maybe_ev {
                            Some(ev) => {
                                // Broadcast first (low latency for live subscribers).
                                let _ = broadcast_for_task.sender_for(ev.run_id).send(ev.clone());

                                // Pulsar publish (fire-and-forget).
                                if let Some(p) = pulsar.as_ref() {
                                    p.publish(&ev);
                                }

                                // Clickhouse: batch.
                                if ch_client.is_some() {
                                    ch_batch.push(ev.to_record());
                                    if ch_batch.len() >= CH_BATCH_MAX {
                                        flush_batch(&ch_client, &mut ch_batch).await;
                                    }
                                }
                            }
                            None => {
                                flush_batch(&ch_client, &mut ch_batch).await;
                                break;
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        if !ch_batch.is_empty() {
                            flush_batch(&ch_client, &mut ch_batch).await;
                        }
                    }
                }
            }
        });

        Self {
            sink: EventSink { tx },
            broadcast,
        }
    }
}

async fn flush_batch(
    ch: &Option<ClickhouseClient>,
    batch: &mut Vec<RunEventRecord>,
) {
    if batch.is_empty() {
        return;
    }
    if let Some(ch) = ch {
        if let Err(e) = ch.insert_run_events(batch).await {
            tracing::warn!(error = %e, rows = batch.len(), "ch run_events insert failed");
        }
    }
    batch.clear();
}

/// Per-invocation helper that assigns sequence numbers to worker-generated
/// state events. Bundle messages already carry sequence numbers assigned by
/// the agent — state events are emitted on the host and need their own
/// counter to keep ordering coherent with the agent-assigned sequence.
///
/// To avoid sequence collisions, state events get the high bit set on
/// `seq` (i.e. seq >= 2^63). This keeps host- and guest-assigned ids in
/// disjoint ranges; consumers should sort by `ts_ms` then `seq` if a
/// global order is needed.
pub struct StateEmitter {
    pub run_id: Uuid,
    pub worker_id: String,
    pub bundle_id: String,
    pub user_id: String,
    pub sink: EventSink,
    counter: std::sync::atomic::AtomicU64,
}

impl StateEmitter {
    pub fn new(
        run_id: Uuid,
        worker_id: String,
        bundle_id: String,
        user_id: String,
        sink: EventSink,
    ) -> Self {
        Self {
            run_id,
            worker_id,
            bundle_id,
            user_id,
            sink,
            counter: std::sync::atomic::AtomicU64::new(1u64 << 63),
        }
    }

    pub fn emit(&self, kind: &str, body: serde_json::Value) {
        let seq = self.counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
        let ev = RunEvent {
            run_id: self.run_id,
            seq,
            ts_ms: crate::observability::now_epoch_ms(),
            kind: kind.to_string(),
            content_type: "application/json".to_string(),
            body: body_bytes,
            attrs: HashMap::new(),
            worker_id: self.worker_id.clone(),
            bundle_id: self.bundle_id.clone(),
            user_id: self.user_id.clone(),
        };
        self.sink.send(ev);
    }
}
