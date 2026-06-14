//! Admin artifact management handlers.
//!
//! - GET /admin/artifacts — list all artifacts across all users (paginated)
//! - DELETE /admin/sessions/{id}/artifacts/{*key} — delete any artifact

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use dimension_store::AuthenticatedUser;

use crate::handlers::artifacts::{validate_artifact_key, ArtifactResponse};
use crate::models::error::AppError;
use crate::server::AppState;

use super::MessageResponse;

// ---------------------------------------------------------------------------
// Query parameter structs
// ---------------------------------------------------------------------------

/// Query params for GET /admin/artifacts.
#[derive(Debug, Deserialize)]
pub struct AdminArtifactsQuery {
    /// Opaque cursor from previous page response.
    pub cursor: Option<String>,
    /// Page size (default 50, max 100).
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Response for GET /admin/artifacts.
#[derive(Serialize)]
pub struct AdminArtifactListResponse {
    pub artifacts: Vec<ArtifactResponse>,
    pub next_cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /admin/artifacts — list all artifacts across all users.
///
/// Supports cursor-based pagination (limit default 50, max 100).
pub async fn admin_list_artifacts_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Query(params): Query<AdminArtifactsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.unwrap_or(50).min(100);

    let (artifacts, next_cursor) = state
        .artifact_store
        .admin_list_artifacts(params.cursor.as_deref(), limit)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    Ok(Json(AdminArtifactListResponse {
        artifacts: artifacts.into_iter().map(ArtifactResponse::from).collect(),
        next_cursor,
    }))
}

/// DELETE /admin/sessions/{id}/artifacts/{*key} — delete an artifact.
///
/// First looks up the session (without user scoping) to get the owner's user_id,
/// then calls delete_artifact with that user_id so the object storage path is correct.
///
/// Returns 404 if the session does not exist.
/// Returns 400 for path traversal keys (.., \0, leading /).
pub async fn admin_delete_artifact_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path((session_id, key)): Path<(Uuid, String)>,
) -> Result<impl IntoResponse, AppError> {
    // Validate key for path traversal.
    validate_artifact_key(&key)?;

    // Look up the session (admin scope, no user_id filter) to get the owner's user_id.
    let session = state
        .session_store
        .admin_get_session(session_id)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?
        .ok_or_else(|| AppError::NotFound("session not found".into()))?;

    let user_id = session
        .user_id
        .ok_or_else(|| AppError::Internal(Box::new(std::io::Error::other(
            "session has no owner user_id",
        ))))?;

    // Require storage client for object deletion.
    let storage_client = state
        .storage_client
        .as_ref()
        .ok_or_else(|| AppError::BadRequest("object storage not configured".into()))?;

    let operator = storage_client
        .operator_for_session(
            &user_id.to_string(),
            &session.bundle_id,
            &session_id.to_string(),
        )
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    state
        .artifact_store
        .delete_artifact(&operator, session_id, user_id, &key)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    Ok((
        StatusCode::OK,
        Json(MessageResponse {
            message: "artifact deleted".into(),
        }),
    ))
}
