//! Admin task management handlers.
//!
//! - GET /admin/tasks — list all tasks across all users (paginated, optional status filter)
//! - POST /admin/tasks/{id}/retry — reset a failed/cancelled task to pending

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use dimension_store::{AuthenticatedUser, Task};

use crate::models::error::AppError;
use crate::server::AppState;

use super::MessageResponse;

// ---------------------------------------------------------------------------
// Query parameter structs
// ---------------------------------------------------------------------------

/// Query params for GET /admin/tasks.
#[derive(Debug, Deserialize)]
pub struct AdminTasksQuery {
    /// Optional status filter (e.g. "pending", "running", "failed", "cancelled", "completed").
    pub status: Option<String>,
    /// Opaque cursor from previous page response.
    pub cursor: Option<String>,
    /// Page size (default 50, max 200).
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// A task item in the admin list response.
#[derive(Debug, Serialize)]
pub struct AdminTaskItem {
    pub id: Uuid,
    pub user_id: Uuid,
    pub bundle_id: String,
    pub goal: String,
    pub status: String,
    pub trigger_type: String,
    pub iteration_count: i32,
    pub max_iterations: i32,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

impl From<Task> for AdminTaskItem {
    fn from(t: Task) -> Self {
        AdminTaskItem {
            id: t.id,
            user_id: t.user_id,
            bundle_id: t.bundle_id,
            goal: t.goal,
            status: t.status,
            trigger_type: t.trigger_type,
            iteration_count: t.iteration_count,
            max_iterations: t.max_iterations as i32,
            created_at: t.created_at.to_rfc3339(),
            updated_at: t.updated_at.to_rfc3339(),
            started_at: t.started_at.map(|dt| dt.to_rfc3339()),
            completed_at: t.completed_at.map(|dt| dt.to_rfc3339()),
        }
    }
}

/// Response for GET /admin/tasks.
#[derive(Debug, Serialize)]
pub struct AdminTaskListResponse {
    pub tasks: Vec<AdminTaskItem>,
    pub next_cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /admin/tasks — list all tasks across all users.
///
/// Optional query params:
/// - `status` — filter by task status
/// - `cursor` — opaque pagination cursor
/// - `limit` — max results (default 50, max 200)
pub async fn admin_list_tasks_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Query(params): Query<AdminTasksQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.unwrap_or(50).min(200);

    let (tasks, next_cursor) = state
        .task_store
        .admin_list_tasks(params.status.as_deref(), params.cursor.as_deref(), limit)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    Ok(Json(AdminTaskListResponse {
        tasks: tasks.into_iter().map(AdminTaskItem::from).collect(),
        next_cursor,
    }))
}

/// POST /admin/tasks/{id}/retry — reset a failed or cancelled task to pending.
///
/// Returns 400 if the task is not in a retryable state (failed or cancelled).
/// Returns 404 if the task does not exist.
pub async fn admin_retry_task_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(task_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    // Verify the task exists first (admin scope).
    let task = state
        .task_store
        .admin_get_task(task_id)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?
        .ok_or_else(|| AppError::NotFound(format!("task {task_id} not found")))?;

    // Verify it's in a retryable state.
    if task.status != "failed" && task.status != "cancelled" {
        return Err(AppError::BadRequest(format!(
            "task is not in a retryable state: current status is '{}'",
            task.status
        )));
    }

    state
        .task_store
        .retry_task(task_id)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    Ok((
        StatusCode::OK,
        Json(MessageResponse {
            message: "task queued for retry".into(),
        }),
    ))
}
