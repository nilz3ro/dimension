//! User-facing storage handlers for per-bundle object storage.
//!
//! - GET /bundles/{id}/storage -- list objects with prefix filtering and cursor pagination
//! - GET /bundles/{id}/storage/{*key} -- get object raw bytes
//! - PUT /bundles/{id}/storage/{*key} -- store object (quota enforced)
//! - DELETE /bundles/{id}/storage/{*key} -- delete object (idempotent)
//!
//! All handlers require `capabilities.storage == true` in the bundle manifest.
//! All handlers require `state.storage_client` to be `Some`.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, Response, StatusCode};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use opendal::EntryMode;
use serde::{Deserialize, Serialize};

use dimension_store::AuthenticatedUser;
use hyphae_core::manifest::CapabilitiesSection;
use hyphae_core::registry::Registry;

use crate::models::error::AppError;
use crate::server::AppState;

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Metadata for a single stored object.
#[derive(Debug, Serialize)]
pub struct StorageObjectMeta {
    pub key: String,
    pub size: u64,
    pub content_type: Option<String>,
}

/// Response for GET /bundles/{id}/storage.
#[derive(Debug, Serialize)]
pub struct ListStorageResponse {
    pub objects: Vec<StorageObjectMeta>,
    pub next_cursor: Option<String>,
}

/// Response for PUT /bundles/{id}/storage/{*key}.
#[derive(Debug, Serialize)]
pub struct PutStorageResponse {
    pub key: String,
    pub size: u64,
}

/// Response for DELETE /bundles/{id}/storage/{*key}.
#[derive(Debug, Serialize)]
pub struct DeleteStorageResponse {
    pub message: &'static str,
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListStorageQuery {
    pub prefix: Option<String>,
    /// Cursor for pagination: maps to opendal start_after.
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

// ---------------------------------------------------------------------------
// Helper: load bundle capabilities from registry
// ---------------------------------------------------------------------------

/// Open registry and get capabilities for a bundle.
///
/// Returns `None` if the bundle has no manifest or capabilities section.
/// All storage handlers treat `None` as capabilities not enabled (deny).
async fn get_bundle_capabilities(
    registry_path: &std::path::Path,
    bundle_id: &str,
) -> Result<Option<CapabilitiesSection>, AppError> {
    let registry_path = registry_path.to_path_buf();
    let bundle_id = bundle_id.to_string();
    tokio::task::spawn_blocking(move || {
        let registry = Registry::open(&registry_path)
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        let (name, tag) = hyphae_core::registry::parse_image_ref(&bundle_id);
        let image = registry
            .find_by_name_tag(&name, &tag)
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        let caps = image.and_then(|img| {
            img.manifest_capabilities
                .as_deref()
                .and_then(|json| serde_json::from_str::<CapabilitiesSection>(json).ok())
        });
        Ok::<Option<CapabilitiesSection>, AppError>(caps)
    })
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))?
}

/// Require storage capability. Returns `Forbidden` if not enabled.
async fn require_storage_capability(
    registry_path: &std::path::Path,
    bundle_id: &str,
) -> Result<(), AppError> {
    let caps = get_bundle_capabilities(registry_path, bundle_id).await?;
    let enabled = caps.map(|c| c.storage).unwrap_or(false);
    if !enabled {
        return Err(AppError::Forbidden(
            "storage capability not enabled for this bundle".into(),
        ));
    }
    Ok(())
}

/// Map an opendal error to an AppError.
fn map_opendal_error(e: opendal::Error) -> AppError {
    if e.kind() == opendal::ErrorKind::NotFound {
        AppError::NotFound(format!("object not found: {}", e))
    } else {
        AppError::Internal(Box::new(e))
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /bundles/{id}/storage -- list objects with optional prefix filter and cursor pagination.
pub async fn list_storage_handler(
    State(state): State<AppState>,
    Path(bundle_id): Path<String>,
    Extension(user): Extension<AuthenticatedUser>,
    Query(query): Query<ListStorageQuery>,
) -> Result<impl IntoResponse, AppError> {
    // Check storage is configured.
    let storage_client = state
        .storage_client
        .as_ref()
        .ok_or_else(|| AppError::Internal(Box::new(std::io::Error::other("storage not configured"))))?;

    // Check capability gate.
    require_storage_capability(&state.registry_path, &bundle_id).await?;

    let user_id_str = user.user_id.to_string();
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
        .map_err(map_opendal_error)?;

    // Filter to FILE entries only (exclude DIR entries).
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

    Ok((StatusCode::OK, Json(ListStorageResponse { objects, next_cursor })))
}

/// GET /bundles/{id}/storage/{*key} -- get raw object bytes.
pub async fn get_storage_handler(
    State(state): State<AppState>,
    Path((bundle_id, key)): Path<(String, String)>,
    Extension(user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, AppError> {
    // Check storage is configured.
    let storage_client = state
        .storage_client
        .as_ref()
        .ok_or_else(|| AppError::Internal(Box::new(std::io::Error::other("storage not configured"))))?;

    // Check capability gate.
    require_storage_capability(&state.registry_path, &bundle_id).await?;

    let user_id_str = user.user_id.to_string();
    let op = storage_client
        .operator_for(&user_id_str, &bundle_id)
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    // Get content-type via stat.
    let stat = op.stat(&key).await.map_err(map_opendal_error)?;
    let content_type = stat
        .content_type()
        .map(str::to_string)
        .unwrap_or_else(|| "application/octet-stream".to_string());

    // Read the raw bytes.
    let bytes = op.read(&key).await.map_err(map_opendal_error)?;

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(axum::body::Body::from(bytes.to_bytes()))
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    Ok(response)
}

/// PUT /bundles/{id}/storage/{*key} -- store object with quota enforcement.
pub async fn put_storage_handler(
    State(state): State<AppState>,
    Path((bundle_id, key)): Path<(String, String)>,
    Extension(user): Extension<AuthenticatedUser>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AppError> {
    // Check storage is configured.
    let storage_client = state
        .storage_client
        .as_ref()
        .ok_or_else(|| AppError::Internal(Box::new(std::io::Error::other("storage not configured"))))?;

    // Check capability gate.
    require_storage_capability(&state.registry_path, &bundle_id).await?;

    let user_id_str = user.user_id.to_string();
    let op = storage_client
        .operator_for(&user_id_str, &bundle_id)
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    let new_size = body.len() as i64;

    // Check quota: get current usage and quota limit.
    let (bytes_used, quota_bytes) = state
        .storage_store
        .get_storage_info(user.user_id, &bundle_id)
        .await?;

    // Get old size if object already exists (for delta tracking on replacement).
    let old_size: i64 = match op.stat(&key).await {
        Ok(meta) => meta.content_length() as i64,
        Err(e) if e.kind() == opendal::ErrorKind::NotFound => 0,
        Err(e) => return Err(map_opendal_error(e)),
    };

    // Quota check: (bytes_used - old_size + new_size) > quota_bytes
    if bytes_used - old_size + new_size > quota_bytes {
        return Err(AppError::QuotaExceeded(format!(
            "storage quota of {} bytes exceeded",
            quota_bytes
        )));
    }

    // Extract Content-Type from request header, defaulting to octet-stream.
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    // Write the object.
    op.write_with(&key, body.to_vec())
        .content_type(&content_type)
        .await
        .map_err(map_opendal_error)?;

    // Update quota tracking: delta = new_size - old_size.
    let delta = new_size - old_size;
    if delta > 0 {
        state
            .storage_store
            .increment_bytes_used(user.user_id, &bundle_id, delta)
            .await?;
    } else if delta < 0 {
        state
            .storage_store
            .decrement_bytes_used(user.user_id, &bundle_id, -delta)
            .await?;
    }

    Ok((
        StatusCode::OK,
        Json(PutStorageResponse {
            key,
            size: new_size as u64,
        }),
    ))
}

/// DELETE /bundles/{id}/storage/{*key} -- delete object (idempotent).
pub async fn delete_storage_handler(
    State(state): State<AppState>,
    Path((bundle_id, key)): Path<(String, String)>,
    Extension(user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, AppError> {
    // Check storage is configured.
    let storage_client = state
        .storage_client
        .as_ref()
        .ok_or_else(|| AppError::Internal(Box::new(std::io::Error::other("storage not configured"))))?;

    // Check capability gate.
    require_storage_capability(&state.registry_path, &bundle_id).await?;

    let user_id_str = user.user_id.to_string();
    let op = storage_client
        .operator_for(&user_id_str, &bundle_id)
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    // Stat first to get size for quota tracking; if NotFound, return success (idempotent).
    let size: u64 = match op.stat(&key).await {
        Ok(meta) => meta.content_length(),
        Err(e) if e.kind() == opendal::ErrorKind::NotFound => {
            return Ok((StatusCode::OK, Json(DeleteStorageResponse { message: "deleted" })));
        }
        Err(e) => return Err(map_opendal_error(e)),
    };

    // Delete the object.
    op.delete(&key).await.map_err(map_opendal_error)?;

    // Decrement bytes_used.
    if size > 0 {
        state
            .storage_store
            .decrement_bytes_used(user.user_id, &bundle_id, size as i64)
            .await?;
    }

    Ok((StatusCode::OK, Json(DeleteStorageResponse { message: "deleted" })))
}
