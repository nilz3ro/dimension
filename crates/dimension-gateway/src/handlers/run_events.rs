//! `GET /runs/:run_id/events` — Server-Sent Events stream of run events.
//!
//! Sources, in order:
//!   1. Clickhouse replay for `[from_seq, latest)` (catch-up on missed events).
//!   2. Live tail from Pulsar (filtered by `run_id` partition key). When the
//!      gateway has no Pulsar client configured, this step is skipped — the
//!      client receives the historical replay and then the stream ends.
//!
//! Auth: bearer auth via the standard middleware. Ownership is enforced by
//! cross-referencing the run's `user_id` in the `invocations` Clickhouse
//! table; admins bypass the check.
//!
//! Reconnection: clients can pass `Last-Event-ID: <seq>` (standard SSE
//! reconnection header) or the `?from_seq=N` query param. We honor the
//! header first.

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Extension;
use futures::stream::{self, Stream, StreamExt};
use serde::Deserialize;
use uuid::Uuid;

use dimension_store::AuthenticatedUser;

use crate::models::error::AppError;
use crate::server::AppState;

const REPLAY_LIMIT: u64 = 10_000;

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub from_seq: Option<u64>,
}

pub async fn run_events_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(run_id_str): Path<String>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let run_id = run_id_str
        .parse::<Uuid>()
        .map_err(|_| AppError::BadRequest(format!("invalid run_id: '{run_id_str}'")))?;

    // Ownership: look up the invocation row; admins skip the check.
    let is_admin = user.role == dimension_store::UserRole::Admin;
    let ch_client = state.clickhouse_client.as_ref().ok_or_else(|| {
        AppError::ServiceUnavailable("clickhouse not configured".into())
    })?;

    if !is_admin {
        // The invocations row may not exist yet for a freshly-dispatched
        // run — the worker writes it on completion. Until then we trust
        // the bearer principal; deny if a row exists with a different
        // user_id.
        let owner: Result<Option<String>, _> = ch_client
            .query("SELECT user_id FROM invocations WHERE invocation_id = ? LIMIT 1")
            .bind(run_id)
            .fetch_optional::<String>()
            .await;
        if let Ok(Some(owner_id)) = owner {
            if owner_id != user.user_id.to_string() {
                return Err(AppError::Forbidden("run owned by another user".into()));
            }
        }
    }

    // Determine resume position: Last-Event-ID header overrides ?from_seq.
    let from_seq = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .or(query.from_seq)
        .unwrap_or(0);

    // ── 1. Clickhouse replay ───────────────────────────────────────────
    let mut replay = ch_client
        .query(
            "SELECT run_id, seq, toUnixTimestamp64Milli(ts) AS ts, kind, \
             content_type, body, attrs, worker_id, bundle_id, user_id \
             FROM run_events WHERE run_id = ? AND seq >= ? \
             ORDER BY seq LIMIT ?",
        )
        .bind(run_id)
        .bind(from_seq)
        .bind(REPLAY_LIMIT)
        .fetch_all::<crate::handlers::run_events::RunEventRow>()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, run_id = %run_id, "ch run_events query failed");
            AppError::Internal(Box::new(e))
        })?;

    // If the run already finished, the terminal `state` event is in the
    // replay. Truncate at it and skip the live tail so the stream ends
    // promptly instead of waiting forever for messages that won't come.
    let terminal_in_replay = replay
        .iter()
        .position(|r| is_terminal(&r.kind, &body_to_json(&r.content_type, r.body.clone())));
    if let Some(i) = terminal_in_replay {
        replay.truncate(i + 1);
    }

    let replay_stream = stream::iter(replay).map(row_to_sse);

    // ── 2. Live Pulsar tail (optional) ─────────────────────────────────
    let live_stream = if terminal_in_replay.is_some() {
        None
    } else if let Some(pulsar) = state.pulsar_client.clone() {
        let sub_suffix = format!("{}-{}", run_id.simple(), Uuid::new_v4().simple());
        match pulsar.subscribe_run_events(&sub_suffix).await {
            Ok(consumer) => Some(pulsar_stream(consumer, run_id)),
            Err(e) => {
                tracing::warn!(error = %e, run_id = %run_id, "pulsar subscribe failed");
                None
            }
        }
    } else {
        None
    };

    let combined: std::pin::Pin<
        Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>,
    > = if let Some(live) = live_stream {
        Box::pin(replay_stream.chain(live))
    } else {
        Box::pin(replay_stream)
    };

    Ok(Sse::new(combined).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    ))
}

#[derive(Debug, Clone, serde::Deserialize, clickhouse::Row)]
pub struct RunEventRow {
    #[serde(with = "clickhouse::serde::uuid")]
    pub run_id: Uuid,
    pub seq: u64,
    pub ts: i64,
    pub kind: String,
    pub content_type: String,
    pub body: String,
    pub attrs: Vec<(String, String)>,
    pub worker_id: String,
    pub bundle_id: String,
    pub user_id: String,
}

/// Render a `body` string from storage. `application/json` bodies become real
/// nested JSON values so consumers parse once; everything else stays a string.
fn body_to_json(content_type: &str, body: String) -> serde_json::Value {
    if content_type.starts_with("application/json") {
        serde_json::from_str(&body).unwrap_or(serde_json::Value::String(body))
    } else {
        serde_json::Value::String(body)
    }
}

/// The run's final lifecycle event — the last thing before we close the stream.
fn is_terminal(kind: &str, body: &serde_json::Value) -> bool {
    kind == "state" && body.get("phase").and_then(|v| v.as_str()) == Some("completed")
}

fn row_to_sse(row: RunEventRow) -> Result<Event, std::convert::Infallible> {
    // `kind` and `seq` are carried on the SSE `event:`/`id:` lines, so we omit
    // them from the data payload to keep the envelope slim.
    let body = body_to_json(&row.content_type, row.body);
    let json = serde_json::json!({
        "run_id": row.run_id.to_string(),
        "ts_ms": row.ts,
        "content_type": row.content_type,
        "body": body,
        "attrs": row.attrs.into_iter().collect::<std::collections::HashMap<_, _>>(),
        "worker_id": row.worker_id,
        "bundle_id": row.bundle_id,
        "user_id": row.user_id,
    });
    Ok(Event::default()
        .id(row.seq.to_string())
        .event(row.kind)
        .data(json.to_string()))
}

fn pulsar_stream(
    mut consumer: pulsar::Consumer<Vec<u8>, pulsar::TokioExecutor>,
    run_id: Uuid,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> + Send {
    let run_id_str = run_id.to_string();
    async_stream::stream! {
        while let Some(msg) = consumer.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "pulsar receive error");
                    continue;
                }
            };
            // Filter by partition key (run_id).
            let key_matches = msg.key().as_deref() == Some(run_id_str.as_str());
            let _ = consumer.ack(&msg).await;
            if !key_matches {
                continue;
            }
            let body: Result<crate::pulsar::PulsarRunEvent, _> =
                serde_json::from_slice(&msg.payload.data);
            let ev = match body {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = %e, "pulsar message decode failed");
                    continue;
                }
            };
            let body = body_to_json(&ev.content_type, ev.body);
            let terminal = is_terminal(&ev.kind, &body);
            let json = serde_json::to_string(&serde_json::json!({
                "run_id": ev.run_id,
                "ts_ms": ev.ts_ms,
                "content_type": ev.content_type,
                "body": body,
                "attrs": ev.attrs,
                "worker_id": ev.worker_id,
                "bundle_id": ev.bundle_id,
                "user_id": ev.user_id,
            }))
            .unwrap_or_default();
            yield Ok(Event::default()
                .id(ev.seq.to_string())
                .event(ev.kind)
                .data(json));
            // Run finished: emit the terminal event, then end the stream so
            // the client sees EOF instead of hanging on the live tail.
            if terminal {
                break;
            }
        }
    }
}
