//! GET /admin/logs/tail — SSE log streaming endpoint.
//!
//! Streams gateway log lines as Server-Sent Events. Each event `data:` field
//! contains a single-line JSON object with `level`, `target`, and `fields`.
//!
//! A ring buffer of the last 1000 lines is replayed immediately when a new
//! subscriber connects, so operators see recent history without waiting.
//!
//! Optional query params (substring filters applied on the gateway side):
//! - `request_id` — only lines whose JSON contains this string
//! - `session`    — only lines whose JSON contains this string
//! - `bundle`     — only lines whose JSON contains this string

use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use tokio_stream::wrappers::BroadcastStream;

use crate::server::AppState;

/// Query parameters for the log tailing endpoint.
#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    /// Substring filter: only emit lines containing this request ID.
    pub request_id: Option<String>,
    /// Substring filter: only emit lines containing this session UUID.
    pub session: Option<String>,
    /// Substring filter: only emit lines containing this bundle name.
    pub bundle: Option<String>,
}

/// GET /admin/logs/tail — stream gateway log lines as SSE.
///
/// Immediately replays the recent ring-buffer contents (up to 1000 lines),
/// then streams new events as they arrive. The stream never ends until the
/// client disconnects or the server shuts down.
///
/// Filters are applied as substring matches on the raw JSON line, which means
/// they work for any field value embedded in the log event.
pub async fn logs_tail_handler(
    State(state): State<AppState>,
    Query(query): Query<LogsQuery>,
) -> impl IntoResponse {
    let broadcaster = state.log_broadcaster.clone();

    // Snapshot recent lines before subscribing so we do not miss events
    // produced between the snapshot and the subscribe call.
    let recent = broadcaster.recent();
    let rx = broadcaster.subscribe();

    // Chain: recent history → live broadcast stream.
    // BroadcastStream yields Result<String, RecvError>; we drop lagged errors.
    let recent_stream = stream::iter(recent);
    let live_stream = BroadcastStream::new(rx).filter_map(|res| async move { res.ok() });

    let combined = recent_stream.chain(live_stream);

    // Clone filter values so the closure can own them.
    let request_id = query.request_id.clone();
    let session = query.session.clone();
    let bundle = query.bundle.clone();

    let filtered = combined.filter(move |line| {
        let keep = {
            if let Some(ref rid) = request_id {
                if !line.contains(rid.as_str()) {
                    return futures::future::ready(false);
                }
            }
            if let Some(ref sid) = session {
                if !line.contains(sid.as_str()) {
                    return futures::future::ready(false);
                }
            }
            if let Some(ref bid) = bundle {
                if !line.contains(bid.as_str()) {
                    return futures::future::ready(false);
                }
            }
            true
        };
        futures::future::ready(keep)
    });

    let sse_stream = filtered.map(|line| {
        Ok::<Event, std::convert::Infallible>(Event::default().data(line))
    });

    Sse::new(sse_stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    )
}
