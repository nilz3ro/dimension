//! Custom HTTP trace layer with request ID propagation.
//!
//! Provides a [`TraceLayer`] configured with a `MakeSpan` closure that reads
//! the `x-request-id` header (set by [`SetRequestIdLayer`] earlier in the
//! middleware stack) and embeds it in the root `http_request` span. This
//! ensures every log line for a request carries the same request ID that
//! appears in the `x-request-id` response header.
//!
//! The span also records the HTTP response status code via `on_response`.

use std::time::Duration;

use axum::http;
use tower_http::classify::{ServerErrorsAsFailures, ServerErrorsFailureClass, SharedClassifier};
use tower_http::trace::{
    DefaultOnBodyChunk, DefaultOnEos, DefaultOnRequest, MakeSpan, OnFailure, OnResponse, TraceLayer,
};
use tracing::Span;

/// The full concrete type returned by [`trace_layer`].
pub type HttpTraceLayer = TraceLayer<
    SharedClassifier<ServerErrorsAsFailures>,
    RequestIdMakeSpan,
    DefaultOnRequest,
    RequestIdOnResponse,
    DefaultOnBodyChunk,
    DefaultOnEos,
    RequestIdOnFailure,
>;

/// Create the HTTP trace layer with custom span creation.
///
/// The returned layer:
/// - Creates an `http_request` span with `request_id`, `method`, `uri`, and
///   an initially-empty `status` field.
/// - Records the response status code on the span when the response is sent.
/// - Logs response latency at INFO level.
/// - Logs failures at ERROR level with latency.
///
/// # Middleware ordering
///
/// This layer must be placed **after** `SetRequestIdLayer` (so the
/// `x-request-id` header exists) and **before** `PropagateRequestIdLayer`
/// in the `ServiceBuilder` stack.
pub fn trace_layer() -> HttpTraceLayer {
    TraceLayer::new_for_http()
        .make_span_with(RequestIdMakeSpan)
        .on_response(RequestIdOnResponse)
        .on_failure(RequestIdOnFailure)
}

/// Custom [`MakeSpan`] that reads the `x-request-id` header set by
/// `SetRequestIdLayer` and includes it in the span fields.
#[derive(Clone, Debug)]
pub struct RequestIdMakeSpan;

impl<B> MakeSpan<B> for RequestIdMakeSpan {
    fn make_span(&mut self, request: &http::Request<B>) -> Span {
        let request_id = request
            .headers()
            .get("x-request-id")
            .and_then(|v: &http::HeaderValue| v.to_str().ok())
            .unwrap_or("unknown");

        tracing::info_span!(
            "http_request",
            request_id = %request_id,
            method = %request.method(),
            uri = %request.uri(),
            status = tracing::field::Empty,
        )
    }
}

/// Records the HTTP status code on the span and logs response latency.
#[derive(Clone, Debug)]
pub struct RequestIdOnResponse;

impl<B> OnResponse<B> for RequestIdOnResponse {
    fn on_response(self, response: &http::Response<B>, latency: Duration, span: &Span) {
        span.record("status", response.status().as_u16());
        tracing::info!(latency_ms = latency.as_millis(), "response");
    }
}

/// Logs request failures with error details and latency.
#[derive(Clone, Debug)]
pub struct RequestIdOnFailure;

impl OnFailure<ServerErrorsFailureClass> for RequestIdOnFailure {
    fn on_failure(
        &mut self,
        failure: ServerErrorsFailureClass,
        latency: Duration,
        _span: &Span,
    ) {
        tracing::error!(
            error = %failure,
            latency_ms = latency.as_millis(),
            "request failed"
        );
    }
}
