//! Admin API key management handlers.
//!
//! All handlers require an authenticated admin user (enforced by `admin_middleware`
//! via `route_layer` on the admin sub-router).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use dimension_store::AuthenticatedUser;
use uuid::Uuid;

use crate::models::error::AppError;
use crate::server::AppState;

use super::{CreateKeyRequest, CreateKeyResponse, MessageResponse};

/// POST /admin/users/{id}/keys
///
/// Creates an additional API key for the specified user. Returns 201 Created
/// with the key record and plaintext key value (shown once; never retrievable again).
pub async fn create_key_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Json(body): Json<CreateKeyRequest>,
) -> Result<impl IntoResponse, AppError> {
    let (key, api_key) = state
        .user_store
        .create_key(user_id, body.label.as_deref())
        .await?;

    let response = CreateKeyResponse {
        key_id: key.id,
        key_prefix: key.key_prefix,
        label: key.label,
        api_key,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// GET /admin/users/{id}/keys
///
/// Lists all API keys for the specified user. Returns 200 OK with key metadata.
/// The `key_hash` field is never included in the response.
pub async fn list_keys_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let keys = state.user_store.list_keys_for_user(user_id).await?;
    let items: Vec<serde_json::Value> = keys
        .iter()
        .map(|k| {
            serde_json::json!({
                "key_id": k.id,
                "key_prefix": k.key_prefix,
                "label": k.label,
                "created_at": k.created_at.to_rfc3339(),
                "revoked": k.revoked_at.is_some(),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "keys": items })))
}

/// POST /admin/users/{id}/keys/{kid}/revoke
///
/// Revokes a specific API key. The `user_id` path parameter is accepted but
/// not used in the revocation (key IDs are globally unique). Returns 200 OK.
pub async fn revoke_key_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path((_user_id, key_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    state.user_store.revoke_key(key_id).await?;

    Ok(Json(MessageResponse {
        message: "key revoked".into(),
    }))
}
