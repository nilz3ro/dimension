//! Invocation observability HTTP handlers.
//!
//! - [`get_invocation_handler`]: GET /invocations/{id} — retrieve invocation metadata + optional logs

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::error::AppError;
use crate::server::AppState;

/// Query parameters for GET /invocations/{id}.
#[derive(Debug, Deserialize)]
pub struct InvocationQuery {
    /// When true, fetch log content from MinIO and include in response.
    #[serde(default)]
    pub include_logs: bool,
}

/// Invocation record returned by GET /invocations/{id}.
///
/// Field names and types match the Clickhouse `invocations` table schema
/// defined in dimension-worker's observability module.
#[derive(Debug, Serialize, Deserialize, clickhouse::Row)]
pub struct InvocationResponse {
    #[serde(with = "clickhouse::serde::uuid")]
    pub invocation_id: Uuid,
    pub user_id: String,
    pub bundle_id: String,
    pub worker_id: String,
    pub mode: String,
    pub status: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub log_url: String,
    pub created_at: i64,
    pub completed_at: i64,
    /// Log content (only populated when `?include_logs=true` and logs are available).
    #[serde(skip_deserializing)]
    #[clickhouse(skip)]
    pub logs: Option<String>,
}

/// GET /invocations/{id} — retrieve invocation metadata from Clickhouse.
///
/// Optionally fetches log content from MinIO when `?include_logs=true`.
/// Returns 400 for invalid UUID, 404 when Clickhouse is not configured
/// or the invocation is not found.
pub async fn get_invocation_handler(
    State(state): State<AppState>,
    Path(invocation_id): Path<String>,
    Query(query): Query<InvocationQuery>,
) -> Result<Json<InvocationResponse>, AppError> {
    // Validate UUID format.
    let id = invocation_id.parse::<Uuid>().map_err(|_| {
        AppError::BadRequest(format!("invalid invocation_id: '{invocation_id}'"))
    })?;

    // Require Clickhouse to be configured.
    let ch_client = state.clickhouse_client.as_ref().ok_or_else(|| {
        AppError::NotFound("invocation observability not configured".into())
    })?;

    // Query Clickhouse for the invocation record.
    let mut record = ch_client
        .query("SELECT ?fields FROM invocations WHERE invocation_id = ?")
        .bind(id)
        .fetch_one::<InvocationResponse>()
        .await
        .map_err(|e| {
            // Clickhouse returns RowNotFound for missing records.
            if e.to_string().contains("not enough data")
                || e.to_string().contains("Unexpected EOF")
            {
                AppError::NotFound(format!("invocation '{id}' not found"))
            } else {
                tracing::error!(error = %e, invocation_id = %id, "clickhouse query failed");
                AppError::Internal(Box::new(e))
            }
        })?;

    // Optionally fetch log content from MinIO.
    if query.include_logs && !record.log_url.is_empty() {
        if let Some(ref log_client) = state.log_storage_client {
            let log_path = format!("invocations/{}/output.log", id);
            match log_client.read(&log_path).await {
                Ok(data) => {
                    record.logs = Some(String::from_utf8_lossy(&data.to_vec()).to_string());
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        invocation_id = %id,
                        "failed to fetch invocation logs from MinIO"
                    );
                    // Don't fail the request — return metadata without logs.
                }
            }
        }
    }

    Ok(Json(record))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parsing a non-UUID string should produce a descriptive error.
    #[test]
    fn test_invalid_uuid_parse() {
        let result = "not-a-uuid".parse::<Uuid>();
        assert!(result.is_err());
    }

    /// Verify InvocationResponse serializes correctly with logs = None.
    #[test]
    fn test_invocation_response_serialization_no_logs() {
        let resp = InvocationResponse {
            invocation_id: Uuid::nil(),
            user_id: "u1".into(),
            bundle_id: "b1".into(),
            worker_id: "w1".into(),
            mode: "sync".into(),
            status: "completed".into(),
            exit_code: 0,
            duration_ms: 500,
            log_url: "s3://bucket/path".into(),
            created_at: 1000,
            completed_at: 2000,
            logs: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "completed");
        assert!(json["logs"].is_null());
    }

    /// Verify InvocationResponse serializes correctly with logs present.
    #[test]
    fn test_invocation_response_serialization_with_logs() {
        let resp = InvocationResponse {
            invocation_id: Uuid::nil(),
            user_id: "u1".into(),
            bundle_id: "b1".into(),
            worker_id: "w1".into(),
            mode: "sync".into(),
            status: "completed".into(),
            exit_code: 0,
            duration_ms: 500,
            log_url: "s3://bucket/path".into(),
            created_at: 1000,
            completed_at: 2000,
            logs: Some("hello world\n".into()),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["logs"], "hello world\n");
    }
}
