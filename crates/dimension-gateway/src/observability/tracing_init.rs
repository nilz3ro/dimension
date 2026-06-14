//! Tracing subscriber initialization.
//!
//! Configures the global tracing subscriber with:
//! - **TTY (development):** Human-readable compact format to stderr
//! - **Non-TTY (production):** JSON lines format to stderr
//! - **Filtering:** Controlled by `RUST_LOG` env var; defaults to
//!   `dimension_gateway=info,tower_http=info`
//! - **Span timing:** `FmtSpan::CLOSE` emits busy/idle durations on span close
//! - **Broadcast layer:** Every log event is also sent to an in-memory broadcast
//!   channel so it can be streamed via `GET /admin/logs/tail`.

use std::io::IsTerminal;

use tracing_subscriber::{fmt, EnvFilter, prelude::*};

use crate::observability::log_broadcast::{BroadcastLayer, LogBroadcaster};

/// Initialize the global tracing subscriber.
///
/// Returns a [`LogBroadcaster`] that receives every log event produced by the
/// gateway. The caller (main.rs) stores this in [`AppState`] so the
/// `GET /admin/logs/tail` SSE handler can subscribe to the live stream.
///
/// Must be called once at startup, before any tracing macros are used.
///
/// # Format selection
///
/// - If stderr is a TTY (interactive terminal): compact human-readable output
///   with targets and span close timing.
/// - If stderr is not a TTY (piped/redirected): JSON lines output with current
///   span context and span close timing.
///
/// # Log level filtering
///
/// Reads `RUST_LOG` environment variable. If unset, defaults to:
/// ```text
/// dimension_gateway=info,tower_http=info
/// ```
pub fn init_tracing() -> LogBroadcaster {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("dimension_gateway=info,tower_http=info"));

    let broadcaster = LogBroadcaster::new();
    let broadcast_layer = BroadcastLayer::new(broadcaster.clone());

    if std::io::stderr().is_terminal() {
        // Human-readable output for development.
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                fmt::layer()
                    .with_target(true)
                    .with_span_events(fmt::format::FmtSpan::CLOSE)
                    .with_writer(std::io::stderr),
            )
            .with(broadcast_layer)
            .init();
    } else {
        // JSON output for production.
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(false)
                    .with_target(true)
                    .with_span_events(fmt::format::FmtSpan::CLOSE)
                    .with_writer(std::io::stderr),
            )
            .with(broadcast_layer)
            .init();
    }

    broadcaster
}
