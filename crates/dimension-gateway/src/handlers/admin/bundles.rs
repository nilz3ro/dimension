//! Admin bundle management handlers.
//!
//! - GET /admin/bundles — list all bundles across all users
//! - DELETE /admin/bundles/{id} — delete a bundle (with active-session safety check)
//! - POST /admin/bundles/{id}/rebuild — trigger bundle re-conversion

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use hyphae_core::registry::Registry;
use serde::Serialize;

use dimension_store::AuthenticatedUser;

use crate::models::error::AppError;
use crate::server::AppState;

use super::MessageResponse;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// A bundle record in the admin list response.
#[derive(Debug, Serialize)]
pub struct AdminBundleItem {
    pub id: i64,
    pub name: String,
    pub tag: String,
    pub content_hash: String,
    pub owner_id: Option<String>,
    pub size_bytes: u64,
    pub created_at: i64,
}

/// Response for GET /admin/bundles.
#[derive(Debug, Serialize)]
pub struct AdminBundlesListResponse {
    pub bundles: Vec<AdminBundleItem>,
}

/// Response for DELETE /admin/bundles/{id}.
#[derive(Debug, Serialize)]
pub struct DeleteBundleResponse {
    pub message: String,
}

/// Response for POST /admin/bundles/{id}/rebuild.
#[derive(Debug, Serialize)]
pub struct RebuildBundleResponse {
    pub message: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /admin/bundles — list all bundles across all users.
///
/// Opens the hyphae registry via spawn_blocking and returns all registered bundles.
pub async fn admin_list_bundles_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let registry_path = state.registry_path.clone();

    let images = tokio::task::spawn_blocking(move || {
        let registry = Registry::open(&registry_path)
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        // list_images(owner_id=None, name=None, tag=None, cursor=None) returns all bundles.
        registry
            .list_images(None, None, None, None)
            .map_err(|e| AppError::Internal(Box::new(e)))
    })
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))??;

    let bundles = images
        .into_iter()
        .map(|img| AdminBundleItem {
            id: img.id,
            name: img.name,
            tag: img.tag,
            content_hash: img.content_hash,
            owner_id: img.owner_id,
            size_bytes: img.size_bytes,
            created_at: img.created_at,
        })
        .collect();

    Ok(Json(AdminBundlesListResponse { bundles }))
}

/// DELETE /admin/bundles/{id} — delete a bundle.
///
/// Checks for active sessions using this bundle first.
/// Returns 409 Conflict with the count of active sessions if any exist.
pub async fn admin_delete_bundle_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(bundle_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Check for active sessions using this bundle.
    let sessions = state
        .session_store
        .admin_list_sessions(Some(&bundle_id))
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    if !sessions.is_empty() {
        return Err(AppError::Conflict(format!(
            "cannot delete bundle: {} active session(s) are using it",
            sessions.len()
        )));
    }

    let registry_path = state.registry_path.clone();
    let bundle_id_clone = bundle_id.clone();

    tokio::task::spawn_blocking(move || {
        let registry = Registry::open(&registry_path)
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        // Parse id as i64 (hyphae registry uses integer IDs).
        let id: i64 = bundle_id_clone
            .parse()
            .map_err(|_| AppError::BadRequest("bundle id must be a valid integer".into()))?;
        registry
            .delete_image(id)
            .map_err(|e| AppError::Internal(Box::new(e)))
    })
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))??;

    Ok((
        StatusCode::OK,
        Json(DeleteBundleResponse {
            message: "bundle deleted".into(),
        }),
    ))
}

/// POST /admin/bundles/{id}/rebuild — trigger bundle re-conversion.
///
/// Rebuild is not yet implemented (see Phase 20 research open questions).
/// Returns 501 Not Implemented.
pub async fn admin_rebuild_bundle_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(_state): State<AppState>,
    Path(_bundle_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    Ok((
        StatusCode::NOT_IMPLEMENTED,
        Json(MessageResponse {
            message: "bundle rebuild not yet implemented".into(),
        }),
    ))
}
