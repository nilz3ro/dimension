//! Admin storage handlers for browsing, managing, and setting quotas on any bundle's storage.
//!
//! - GET /admin/storage -- list all bundles with storage stats
//! - GET /admin/storage/{user_id}/{bundle_id} -- list objects in any bundle
//! - GET /admin/storage/{user_id}/{bundle_id}/{*key} -- get any object raw bytes
//! - DELETE /admin/storage/{user_id}/{bundle_id}/{*key} -- delete any object
//! - PUT /admin/storage/{user_id}/{bundle_id}/quota -- set quota for a bundle
//!
//! Admin handlers bypass the bundle capability check (admin has full access).
//! All handlers still require `state.storage_client` to be `Some`.

use axum::extract::{Path, Query, State};
use axum::http::{header, Response, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use opendal::EntryMode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::handlers::storage::StorageObjectMeta;
use crate::models::error::AppError;
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Storage summary for a single bundle in admin list-all response.
#[derive(Debug, Serialize)]
pub struct AdminStorageSummary {
    pub user_id: Uuid,
    pub bundle_id: String,
    pub bytes_used: i64,
    pub quota_bytes: i64,
    pub updated_at: String,
}

/// Response for GET /admin/storage.
#[derive(Debug, Serialize)]
pub struct AdminListAllResponse {
    pub bundles: Vec<AdminStorageSummary>,
}

/// Response for GET /admin/storage/{user_id}/{bundle_id}.
#[derive(Debug, Serialize)]
pub struct AdminListObjectsResponse {
    pub objects: Vec<StorageObjectMeta>,
    pub next_cursor: Option<String>,
}

/// Request body for PUT /admin/storage/{user_id}/{bundle_id}/quota.
#[derive(Debug, Deserialize)]
pub struct SetQuotaRequest {
    pub quota_bytes: i64,
}

/// Response for PUT /admin/storage/{user_id}/{bundle_id}/quota.
#[derive(Debug, Serialize)]
pub struct SetQuotaResponse {
    pub message: String,
    pub quota_bytes: i64,
}

/// Query params for admin list.
#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    pub prefix: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /admin/storage -- list all bundles with their storage stats.
pub async fn admin_list_all_storage_handler(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    // Check storage is configured.
    let _storage_client = state
        .storage_client
        .as_ref()
        .ok_or_else(|| AppError::Internal(Box::new(std::io::Error::other("storage not configured"))))?;

    let records = state.storage_store.list_storage_stats().await?;

    let bundles = records
        .into_iter()
        .map(|r| AdminStorageSummary {
            user_id: r.user_id,
            bundle_id: r.bundle_id,
            bytes_used: r.bytes_used,
            quota_bytes: r.quota_bytes,
            updated_at: r.updated_at.to_rfc3339(),
        })
        .collect();

    Ok((StatusCode::OK, Json(AdminListAllResponse { bundles })))
}

/// GET /admin/storage/{user_id}/{bundle_id} -- list objects in any bundle.
pub async fn admin_list_bundle_storage_handler(
    State(state): State<AppState>,
    Path((user_id, bundle_id)): Path<(Uuid, String)>,
    Query(query): Query<AdminListQuery>,
) -> Result<impl IntoResponse, AppError> {
    // Check storage is configured.
    let storage_client = state
        .storage_client
        .as_ref()
        .ok_or_else(|| AppError::Internal(Box::new(std::io::Error::other("storage not configured"))))?;

    let user_id_str = user_id.to_string();
    // Use operator_for to scope to the user+bundle namespace (same as user handler).
    // Admin bypasses capability check but uses the same namespace isolation.
    let op = storage_client
        .operator_for(&user_id_str, &bundle_id)
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    let prefix = query.prefix.as_deref().unwrap_or("");
    let limit = query.limit.unwrap_or(100).min(1000) as usize;
    let cursor = query.cursor.as_deref().unwrap_or("");

    let entries = op
        .list_with(prefix)
        .start_after(cursor)
        .limit(limit)
        .await
        .map_err(|e| {
            if e.kind() == opendal::ErrorKind::NotFound {
                AppError::NotFound(format!("bundle namespace not found: {}", e))
            } else {
                AppError::Internal(Box::new(e))
            }
        })?;

    // Filter to FILE entries only.
    let file_entries: Vec<_> = entries
        .into_iter()
        .filter(|e| e.metadata().mode() == EntryMode::FILE)
        .collect();

    let next_cursor = if file_entries.len() == limit {
        file_entries.last().map(|e| e.path().to_string())
    } else {
        None
    };

    let objects: Vec<StorageObjectMeta> = file_entries
        .into_iter()
        .map(|e| {
            let meta = e.metadata();
            StorageObjectMeta {
                key: e.path().to_string(),
                size: meta.content_length(),
                content_type: meta.content_type().map(str::to_string),
            }
        })
        .collect();

    Ok((
        StatusCode::OK,
        Json(AdminListObjectsResponse { objects, next_cursor }),
    ))
}

/// GET /admin/storage/{user_id}/{bundle_id}/{*key} -- get raw object bytes from any bundle.
pub async fn admin_get_object_handler(
    State(state): State<AppState>,
    Path((user_id, bundle_id, key)): Path<(Uuid, String, String)>,
) -> Result<impl IntoResponse, AppError> {
    // Check storage is configured.
    let storage_client = state
        .storage_client
        .as_ref()
        .ok_or_else(|| AppError::Internal(Box::new(std::io::Error::other("storage not configured"))))?;

    let user_id_str = user_id.to_string();
    let op = storage_client
        .operator_for(&user_id_str, &bundle_id)
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    // Get content-type via stat.
    let stat = op.stat(&key).await.map_err(|e| {
        if e.kind() == opendal::ErrorKind::NotFound {
            AppError::NotFound(format!("object not found: {key}"))
        } else {
            AppError::Internal(Box::new(e))
        }
    })?;
    let content_type = stat
        .content_type()
        .map(str::to_string)
        .unwrap_or_else(|| "application/octet-stream".to_string());

    // Read raw bytes.
    let bytes = op.read(&key).await.map_err(|e| {
        if e.kind() == opendal::ErrorKind::NotFound {
            AppError::NotFound(format!("object not found: {key}"))
        } else {
            AppError::Internal(Box::new(e))
        }
    })?;

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(axum::body::Body::from(bytes.to_bytes()))
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    Ok(response)
}

/// DELETE /admin/storage/{user_id}/{bundle_id}/{*key} -- delete any object.
pub async fn admin_delete_object_handler(
    State(state): State<AppState>,
    Path((user_id, bundle_id, key)): Path<(Uuid, String, String)>,
) -> Result<impl IntoResponse, AppError> {
    // Check storage is configured.
    let storage_client = state
        .storage_client
        .as_ref()
        .ok_or_else(|| AppError::Internal(Box::new(std::io::Error::other("storage not configured"))))?;

    let user_id_str = user_id.to_string();
    let op = storage_client
        .operator_for(&user_id_str, &bundle_id)
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    // Stat first to get size for quota tracking.
    let size: u64 = match op.stat(&key).await {
        Ok(meta) => meta.content_length(),
        Err(e) if e.kind() == opendal::ErrorKind::NotFound => {
            return Ok((
                StatusCode::OK,
                Json(super::MessageResponse {
                    message: "deleted".to_string(),
                }),
            ));
        }
        Err(e) => return Err(AppError::Internal(Box::new(e))),
    };

    // Delete the object.
    op.delete(&key).await.map_err(|e| AppError::Internal(Box::new(e)))?;

    // Decrement bytes_used for the target bundle.
    if size > 0 {
        state
            .storage_store
            .decrement_bytes_used(user_id, &bundle_id, size as i64)
            .await?;
    }

    Ok((
        StatusCode::OK,
        Json(super::MessageResponse {
            message: "deleted".to_string(),
        }),
    ))
}

/// PUT /admin/storage/{user_id}/{bundle_id}/quota -- set quota for a bundle.
pub async fn admin_set_quota_handler(
    State(state): State<AppState>,
    Path((user_id, bundle_id)): Path<(Uuid, String)>,
    Json(body): Json<SetQuotaRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Check storage is configured.
    let _storage_client = state
        .storage_client
        .as_ref()
        .ok_or_else(|| AppError::Internal(Box::new(std::io::Error::other("storage not configured"))))?;

    // Validate quota_bytes > 0.
    if body.quota_bytes <= 0 {
        return Err(AppError::BadRequest(
            "quota_bytes must be greater than 0".into(),
        ));
    }

    state
        .storage_store
        .set_quota(user_id, &bundle_id, body.quota_bytes)
        .await?;

    Ok((
        StatusCode::OK,
        Json(SetQuotaResponse {
            message: format!("quota set for {bundle_id}"),
            quota_bytes: body.quota_bytes,
        }),
    ))
}
