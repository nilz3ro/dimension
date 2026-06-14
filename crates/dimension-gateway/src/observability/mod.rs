//! Observability infrastructure for the dimension gateway.
//!
//! - [`init_tracing`]: Subscriber initialization with conditional JSON/compact formatting,
//!   returns a [`LogBroadcaster`] for live log streaming via `GET /admin/logs/tail`.
//! - [`trace_layer`]: Custom `TraceLayer` with `MakeSpan` that embeds request ID in spans
//! - [`log_broadcast`]: In-memory broadcast channel + ring buffer for log tailing

mod make_span;
pub mod log_broadcast;
mod tracing_init;

pub use log_broadcast::LogBroadcaster;
pub use make_span::trace_layer;
pub use tracing_init::init_tracing;
