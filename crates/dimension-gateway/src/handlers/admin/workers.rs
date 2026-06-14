//! Admin worker registry handlers.
//!
//! - GET /admin/workers — list registered workers (or empty in single-host mode)
//! - POST /admin/workers/{id}/drain — mark a worker as draining
//! - DELETE /admin/workers/{id} — remove a worker from the registry

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use dimension_store::AuthenticatedUser;

use crate::models::error::AppError;
use crate::server::AppState;

use super::MessageResponse;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// A worker snapshot in the admin list response.
#[derive(Debug, Serialize)]
pub struct AdminWorkerItem {
    pub worker_id: Uuid,
    pub grpc_addr: String,
    pub available_memory_mb: u64,
    pub available_vcpus: u32,
    pub running_vms: i32,
    pub draining: bool,
    /// Elapsed seconds since last health poll update.
    pub last_seen_secs: u64,
    /// Elapsed seconds since last heartbeat (registration POST). None if no heartbeat yet.
    pub last_heartbeat_secs: Option<u64>,
}

/// Response for GET /admin/workers.
#[derive(Debug, Serialize)]
pub struct AdminWorkersListResponse {
    pub workers: Vec<AdminWorkerItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /admin/workers — list all registered workers.
///
/// In single-host mode (worker_registry is None), returns an empty list
/// with an explanatory note.
pub async fn admin_list_workers_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    match &state.worker_registry {
        None => Ok(Json(AdminWorkersListResponse {
            workers: vec![],
            note: Some("worker registry not enabled (single-host mode)".into()),
        })),
        Some(registry) => {
            let workers = registry
                .get_all()
                .into_iter()
                .map(|w| AdminWorkerItem {
                    worker_id: w.worker_id,
                    grpc_addr: w.grpc_addr,
                    available_memory_mb: w.available_memory_mb,
                    available_vcpus: w.available_vcpus,
                    running_vms: w.running_vms,
                    draining: w.draining,
                    last_seen_secs: w.last_seen.elapsed().as_secs(),
                    last_heartbeat_secs: w.last_heartbeat.map(|h| h.elapsed().as_secs()),
                })
                .collect();

            Ok(Json(AdminWorkersListResponse {
                workers,
                note: None,
            }))
        }
    }
}

/// POST /admin/workers/{id}/drain — mark a worker as draining.
///
/// Returns 404 if the worker registry is not enabled or the worker is not found.
pub async fn admin_drain_worker_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(worker_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let registry = state
        .worker_registry
        .as_ref()
        .ok_or_else(|| AppError::NotFound("worker registry not enabled (single-host mode)".into()))?;

    // Verify the worker exists before marking it as draining.
    if registry.get(worker_id).is_none() {
        return Err(AppError::NotFound(format!("worker {worker_id} not found")));
    }

    registry.mark_draining(worker_id);

    Ok((
        StatusCode::OK,
        Json(MessageResponse {
            message: "worker marked as draining".into(),
        }),
    ))
}

/// DELETE /admin/workers/{id} — remove a worker from the registry.
///
/// Returns 404 if the worker registry is not enabled.
pub async fn admin_remove_worker_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(worker_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let registry = state
        .worker_registry
        .as_ref()
        .ok_or_else(|| AppError::NotFound("worker registry not enabled (single-host mode)".into()))?;

    registry.remove(worker_id);

    Ok((
        StatusCode::OK,
        Json(MessageResponse {
            message: "worker removed".into(),
        }),
    ))
}
