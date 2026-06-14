//! Admin session management handlers.
//!
//! - GET /admin/sessions — list all sessions across all users
//! - GET /admin/sessions/{id} — session detail with history
//! - DELETE /admin/sessions/{id} — soft-delete any session
//! - GET /admin/sessions/search — search sessions by keyword/bundle/date

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use dimension_store::AuthenticatedUser;

use crate::models::error::AppError;
use crate::server::AppState;

use super::MessageResponse;

// ---------------------------------------------------------------------------
// Query parameter structs
// ---------------------------------------------------------------------------

/// Query params for GET /admin/sessions.
#[derive(Debug, Deserialize)]
pub struct AdminSessionsQuery {
    /// Optional bundle_id filter.
    pub bundle: Option<String>,
}

/// Query params for GET /admin/sessions/search.
#[derive(Debug, Deserialize)]
pub struct AdminSearchQuery {
    /// Case-insensitive keyword match against message content.
    pub q: Option<String>,
    /// Case-insensitive substring match against bundle_id.
    pub bundle: Option<String>,
    /// ISO 8601 start date (inclusive).
    pub from: Option<String>,
    /// ISO 8601 end date (inclusive).
    pub to: Option<String>,
    /// Max results to return. Default 50, max 200.
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// A session summary in the admin list response.
#[derive(Debug, Serialize)]
pub struct AdminSessionSummary {
    pub id: Uuid,
    pub bundle_id: String,
    pub message_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Response for GET /admin/sessions.
#[derive(Debug, Serialize)]
pub struct AdminSessionsListResponse {
    pub sessions: Vec<AdminSessionSummary>,
}

/// Response for GET /admin/sessions/{id}.
#[derive(Debug, Serialize)]
pub struct AdminSessionDetailResponse {
    pub session: AdminSessionDetail,
    pub messages: Vec<AdminMessageItem>,
}

/// Session detail for the inspect endpoint.
#[derive(Debug, Serialize)]
pub struct AdminSessionDetail {
    pub id: Uuid,
    /// Nullable — legacy sessions may not have a user_id.
    pub user_id: Option<Uuid>,
    pub bundle_id: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A single message in the session history.
#[derive(Debug, Serialize)]
pub struct AdminMessageItem {
    pub id: Uuid,
    pub role: String,
    pub content: String,
    pub is_complete: bool,
    pub created_at: String,
}

/// A single search result.
#[derive(Debug, Serialize)]
pub struct AdminSearchResultItem {
    pub session_id: Uuid,
    pub bundle_id: String,
    pub created_at: String,
    pub match_snippet: Option<String>,
}

/// Response for GET /admin/sessions/search.
#[derive(Debug, Serialize)]
pub struct AdminSearchResponse {
    pub results: Vec<AdminSearchResultItem>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /admin/sessions — list all sessions across all users.
///
/// Optional query param: `bundle` — filter by bundle_id.
pub async fn admin_list_sessions_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Query(params): Query<AdminSessionsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let sessions = state
        .session_store
        .admin_list_sessions(params.bundle.as_deref())
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    let items = sessions
        .into_iter()
        .map(|s| AdminSessionSummary {
            id: s.id,
            bundle_id: s.bundle_id,
            message_count: s.message_count,
            created_at: s.created_at.to_rfc3339(),
            updated_at: s.updated_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(AdminSessionsListResponse { sessions: items }))
}

/// GET /admin/sessions/{id} — session detail with full message history.
///
/// Returns 404 if the session does not exist or is soft-deleted.
pub async fn admin_get_session_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let session = state
        .session_store
        .admin_get_session(session_id)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?
        .ok_or_else(|| AppError::NotFound("session not found".into()))?;

    let messages = state
        .session_store
        .admin_get_history(session_id)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    let session_detail = AdminSessionDetail {
        id: session.id,
        user_id: session.user_id,
        bundle_id: session.bundle_id,
        created_at: session.created_at.to_rfc3339(),
        updated_at: session.updated_at.to_rfc3339(),
    };

    let message_items = messages
        .into_iter()
        .map(|m| AdminMessageItem {
            id: m.id,
            role: m.role.to_string(),
            content: m.content,
            is_complete: m.is_complete,
            created_at: m.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(AdminSessionDetailResponse {
        session: session_detail,
        messages: message_items,
    }))
}

/// DELETE /admin/sessions/{id} — soft-delete any session regardless of owner.
///
/// Returns 404 if the session does not exist or is already deleted.
pub async fn admin_delete_session_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    state
        .session_store
        .admin_soft_delete_session(session_id)
        .await
        .map_err(|e| match e {
            dimension_store::StoreError::SessionNotFound { .. } => {
                AppError::NotFound("session not found".into())
            }
            other => AppError::Internal(Box::new(other)),
        })?;

    Ok((
        StatusCode::OK,
        Json(MessageResponse {
            message: "session deleted".into(),
        }),
    ))
}

/// GET /admin/sessions/search — search sessions by keyword, bundle, and date range.
///
/// Query params:
/// - `q` — keyword search against message content (case-insensitive)
/// - `bundle` — filter by bundle_id substring (case-insensitive)
/// - `from` — sessions created on or after this ISO 8601 timestamp
/// - `to` — sessions created on or before this ISO 8601 timestamp
/// - `limit` — max results (default 50, max 200)
pub async fn admin_search_sessions_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Query(params): Query<AdminSearchQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.unwrap_or(50).min(200);

    // Parse from/to timestamps.
    let from = params
        .from
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|_| AppError::BadRequest(format!("invalid 'from' timestamp: {s}")))
        })
        .transpose()?;

    let to = params
        .to
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|_| AppError::BadRequest(format!("invalid 'to' timestamp: {s}")))
        })
        .transpose()?;

    let results = state
        .session_store
        .admin_search_sessions(
            params.q.as_deref(),
            params.bundle.as_deref(),
            from,
            to,
            limit,
        )
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    let items = results
        .into_iter()
        .map(|r| AdminSearchResultItem {
            session_id: r.session_id,
            bundle_id: r.bundle_id,
            created_at: r.created_at.to_rfc3339(),
            match_snippet: r.match_snippet,
        })
        .collect();

    Ok(Json(AdminSearchResponse { results: items }))
}
