//! Error types for the HTTP API.
//!
//! `AppError` maps to structured JSON error responses with appropriate
//! HTTP status codes. The `Internal` variant intentionally does not leak
//! error details to the client.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// A single validation problem on a specific field.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationProblem {
    /// The field path that failed validation (e.g. "content[0].text").
    pub field: String,
    /// A human-readable description of the problem.
    pub message: String,
}

/// Application-level errors that map to structured HTTP error responses.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Request failed validation. Contains all collected problems.
    #[error("validation failed")]
    Validation(Vec<ValidationProblem>),

    /// Request is missing or has an invalid authentication token.
    #[error("unauthorized")]
    Unauthorized,

    /// Request is malformed (bad JSON, wrong content type, etc.).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// An internal error occurred. The source is logged but not returned.
    #[error("internal error")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Authenticated but not authorized for this resource.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// Resource not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// Resource quota exceeded.
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),

    /// Request payload is too large (upload size limit exceeded).
    #[error("payload too large")]
    PayloadTooLarge,

    /// Resource already exists (duplicate creation attempt).
    #[error("conflict: {0}")]
    Conflict(String),

    /// No backend worker available to handle the request (503).
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    /// Upstream dependency timed out (504).
    #[error("gateway timeout: {0}")]
    GatewayTimeout(String),
}

/// The top-level error response body shape.
///
/// Matches the pattern: `{ "error": { "code": "...", "message": "...", "details": ... } }`
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// The error detail object.
    pub error: ErrorDetail,
}

/// The inner error detail.
#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    /// Machine-readable error code (e.g. "validation_error", "unauthorized").
    pub code: String,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured details (e.g. list of validation problems).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            AppError::Validation(problems) => {
                let details =
                    serde_json::to_value(&problems).unwrap_or(serde_json::Value::Null);
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    ErrorBody {
                        error: ErrorDetail {
                            code: "validation_error".into(),
                            message: format!(
                                "Request validation failed with {} problem(s)",
                                problems.len()
                            ),
                            details: Some(details),
                        },
                    },
                )
            }
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                ErrorBody {
                    error: ErrorDetail {
                        code: "unauthorized".into(),
                        message: "Invalid or missing authentication token".into(),
                        details: None,
                    },
                },
            ),
            AppError::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                ErrorBody {
                    error: ErrorDetail {
                        code: "bad_request".into(),
                        message: msg,
                        details: None,
                    },
                },
            ),
            AppError::Internal(err) => {
                // Log the actual error but don't expose it to the client.
                tracing::error!(error = %err, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorBody {
                        error: ErrorDetail {
                            code: "internal_error".into(),
                            message: "An internal error occurred".into(),
                            details: None,
                        },
                    },
                )
            }
            AppError::Forbidden(msg) => (
                StatusCode::FORBIDDEN,
                ErrorBody {
                    error: ErrorDetail {
                        code: "forbidden".into(),
                        message: if msg.is_empty() {
                            "Admin access required".into()
                        } else {
                            msg
                        },
                        details: None,
                    },
                },
            ),
            AppError::NotFound(msg) => (
                StatusCode::NOT_FOUND,
                ErrorBody {
                    error: ErrorDetail {
                        code: "not_found".into(),
                        message: msg,
                        details: None,
                    },
                },
            ),
            AppError::QuotaExceeded(msg) => (
                StatusCode::TOO_MANY_REQUESTS,
                ErrorBody {
                    error: ErrorDetail {
                        code: "quota_exceeded".into(),
                        message: msg,
                        details: None,
                    },
                },
            ),
            AppError::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorBody {
                    error: ErrorDetail {
                        code: "payload_too_large".into(),
                        message: "Request payload exceeds the allowed size limit".into(),
                        details: None,
                    },
                },
            ),
            AppError::Conflict(msg) => (
                StatusCode::CONFLICT,
                ErrorBody {
                    error: ErrorDetail {
                        code: "conflict".into(),
                        message: msg,
                        details: None,
                    },
                },
            ),
            AppError::ServiceUnavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorBody {
                    error: ErrorDetail {
                        code: "service_unavailable".into(),
                        message: msg,
                        details: None,
                    },
                },
            ),
            AppError::GatewayTimeout(msg) => (
                StatusCode::GATEWAY_TIMEOUT,
                ErrorBody {
                    error: ErrorDetail {
                        code: "gateway_timeout".into(),
                        message: msg,
                        details: None,
                    },
                },
            ),
        };

        (status, axum::Json(body)).into_response()
    }
}

impl From<dimension_store::StoreError> for AppError {
    fn from(e: dimension_store::StoreError) -> Self {
        match e {
            dimension_store::StoreError::UserNotFound { id } => {
                AppError::NotFound(format!("user not found: {id}"))
            }
            dimension_store::StoreError::KeyNotFound => {
                AppError::NotFound("key not found".into())
            }
            dimension_store::StoreError::LastAdminDemotion => {
                AppError::BadRequest("cannot demote or delete the last admin".into())
            }
            dimension_store::StoreError::Duplicate(msg) => AppError::BadRequest(msg),
            dimension_store::StoreError::Conflict(msg) => AppError::Conflict(msg),
            other => AppError::Internal(Box::new(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn response_body(resp: Response) -> (StatusCode, serde_json::Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn validation_error_returns_422_with_problems() {
        let err = AppError::Validation(vec![
            ValidationProblem {
                field: "role".into(),
                message: "must not be empty".into(),
            },
            ValidationProblem {
                field: "content".into(),
                message: "must contain at least one block".into(),
            },
        ]);
        let (status, json) = response_body(err.into_response()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(json["error"]["code"], "validation_error");
        let details = json["error"]["details"].as_array().unwrap();
        assert_eq!(details.len(), 2);
    }

    #[tokio::test]
    async fn unauthorized_returns_401() {
        let err = AppError::Unauthorized;
        let (status, json) = response_body(err.into_response()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["error"]["code"], "unauthorized");
        assert!(json["error"]["details"].is_null());
    }

    #[tokio::test]
    async fn bad_request_returns_400() {
        let err = AppError::BadRequest("invalid JSON".into());
        let (status, json) = response_body(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "bad_request");
        assert_eq!(json["error"]["message"], "invalid JSON");
    }

    #[tokio::test]
    async fn payload_too_large_returns_413() {
        let err = AppError::PayloadTooLarge;
        let (status, json) = response_body(err.into_response()).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(json["error"]["code"], "payload_too_large");
    }

    #[tokio::test]
    async fn not_found_returns_404() {
        let err = AppError::NotFound("bundle 42 not found".into());
        let (status, json) = response_body(err.into_response()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"]["code"], "not_found");
        assert_eq!(json["error"]["message"], "bundle 42 not found");
    }

    #[tokio::test]
    async fn conflict_returns_409() {
        let err = AppError::Conflict("bundle already exists".into());
        let (status, json) = response_body(err.into_response()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "conflict");
        assert_eq!(json["error"]["message"], "bundle already exists");
    }

    #[tokio::test]
    async fn internal_error_does_not_leak_details() {
        let err = AppError::Internal(Box::new(std::io::Error::other(
            "secret database password exposed",
        )));
        let (status, json) = response_body(err.into_response()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["error"]["code"], "internal_error");
        assert_eq!(json["error"]["message"], "An internal error occurred");
        // Must NOT contain the actual error message
        let body_str = json.to_string();
        assert!(!body_str.contains("secret"));
        assert!(!body_str.contains("database"));
    }
}
