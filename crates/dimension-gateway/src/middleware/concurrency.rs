//! Concurrency limit middleware.
//!
//! Enforces a maximum number of concurrent requests by acquiring an
//! [`OwnedSemaphorePermit`] from a shared [`Semaphore`]. Requests that
//! cannot acquire a permit receive an immediate HTTP 503 with a
//! structured JSON error body.
//!
//! The permit is stored in request extensions so the handler can extract
//! it and move it into the spawned VM lifecycle task, where it is held
//! for the VM's full lifetime and released on drop.
//!
//! # Anti-patterns avoided
//!
//! - **Not** using `tower::limit::ConcurrencyLimitLayer` (it queues
//!   instead of rejecting with 503).
//! - **Not** using `SemaphorePermit` (borrowed) -- `OwnedSemaphorePermit`
//!   is required to cross `tokio::spawn` boundaries.
//! - **Not** including a `Retry-After` header (per CONTEXT.md).

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use tokio::sync::Semaphore;

/// Concurrency limit middleware for use with
/// [`axum::middleware::from_fn_with_state`].
///
/// The state type is `Arc<Semaphore>` (not `ConcurrencyController`)
/// because the middleware only needs the semaphore. The controller
/// lives in `AppState` for admin/health use.
///
/// On success, the [`OwnedSemaphorePermit`](tokio::sync::OwnedSemaphorePermit)
/// is inserted into request extensions. Handlers extract it and move it
/// into the spawned task so the permit lives for the VM's full lifetime.
///
/// On failure (semaphore exhausted), returns HTTP 503 with a structured
/// JSON body matching the `{error: {code, message}}` shape used by
/// [`AppError`](crate::models::error::AppError).
pub async fn concurrency_limit_middleware(
    State(semaphore): State<Arc<Semaphore>>,
    mut request: Request,
    next: Next,
) -> Response {
    match semaphore.clone().try_acquire_owned() {
        Ok(permit) => {
            // Store the owned permit in request extensions so the handler
            // can extract it and pass it to the spawned VM task.
            // Wrap in Arc because http::Extensions requires Clone.
            request.extensions_mut().insert(Arc::new(permit));
            next.run(request).await
        }
        Err(_) => {
            // Semaphore exhausted -- immediate 503, no queueing.
            let body = serde_json::json!({
                "error": {
                    "code": "capacity_exceeded",
                    "message": "Server is at capacity. Please try again later."
                }
            });
            (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, middleware, routing::get, Router};
    use http_body_util::BodyExt;
    use tokio::sync::OwnedSemaphorePermit;
    use tower::ServiceExt;

    /// Handler that verifies the permit is in extensions and returns 200.
    async fn test_handler(request: Request) -> impl IntoResponse {
        let has_permit = request
            .extensions()
            .get::<Arc<OwnedSemaphorePermit>>()
            .is_some();
        if has_permit {
            StatusCode::OK
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }

    fn test_app(semaphore: Arc<Semaphore>) -> Router {
        Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(
                semaphore.clone(),
                concurrency_limit_middleware,
            ))
            .with_state(semaphore)
    }

    #[tokio::test]
    async fn permits_request_when_capacity_available() {
        let semaphore = Arc::new(Semaphore::new(10));
        let app = test_app(semaphore);

        let req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn returns_503_when_at_capacity() {
        let semaphore = Arc::new(Semaphore::new(1));

        // Exhaust the single permit.
        let _permit = semaphore.clone().try_acquire_owned().unwrap();

        let app = test_app(semaphore);
        let req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "capacity_exceeded");
        assert_eq!(
            json["error"]["message"],
            "Server is at capacity. Please try again later."
        );
    }

    #[tokio::test]
    async fn permit_stored_in_request_extensions() {
        let semaphore = Arc::new(Semaphore::new(5));
        let app = test_app(semaphore.clone());

        let req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // The handler checks for OwnedSemaphorePermit in extensions
        // and returns 200 if found, 500 if not.
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn permit_released_after_response() {
        let semaphore = Arc::new(Semaphore::new(1));
        let app = test_app(semaphore.clone());

        // First request consumes the permit.
        let req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // After the response is dropped, the permit should be released.
        drop(resp);

        // Second request should succeed.
        let app2 = test_app(semaphore.clone());
        let req2 = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);
    }
}
