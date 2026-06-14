//! Admin user management handlers.
//!
//! All handlers require an authenticated admin user (enforced by `admin_middleware`
//! via `route_layer` on the admin sub-router). The `Extension<AuthenticatedUser>`
//! extractor is used defensively to document the dependency; in practice it is always
//! present because `admin_middleware` already validated it.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use dimension_store::{AuthenticatedUser, UserRole};
use uuid::Uuid;

use crate::models::error::AppError;
use crate::server::AppState;

use super::{
    CreateUserRequest, CreateUserResponse, MessageResponse, UserResponse, UsersListResponse,
};

/// POST /admin/users
///
/// Creates a new user with `UserRole::User`. Returns 201 Created with the user
/// record and the plaintext API key (shown once; never retrievable again).
pub async fn create_user_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Json(body): Json<CreateUserRequest>,
) -> Result<impl IntoResponse, AppError> {
    let name = body.name.trim().to_string();
    if name.is_empty() || name.len() > 255 {
        return Err(AppError::BadRequest(
            "name must be between 1 and 255 characters".into(),
        ));
    }

    let (user, api_key) = state
        .user_store
        .create_user(&name, UserRole::User)
        .await?;

    let response = CreateUserResponse {
        user_id: user.id,
        name: user.name,
        role: user.role.to_string(),
        api_key,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// GET /admin/users
///
/// Lists all active (non-deleted) users. Returns 200 OK with the user list.
pub async fn list_users_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let users = state.user_store.list_users().await?;

    let user_responses: Vec<UserResponse> = users
        .into_iter()
        .map(|u| UserResponse {
            id: u.id,
            name: u.name,
            role: u.role.to_string(),
            created_at: u.created_at.to_rfc3339(),
            quota_max_sessions: u.quota_max_sessions,
            quota_max_bundles: u.quota_max_bundles,
            quota_max_concurrent_vms: u.quota_max_concurrent_vms,
        })
        .collect();

    Ok(Json(UsersListResponse {
        users: user_responses,
    }))
}

/// DELETE /admin/users/{id}
///
/// Soft-deletes a user (sets `deleted_at`, blocks future auth). Returns 200 OK.
pub async fn delete_user_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    state.user_store.soft_delete_user(user_id).await?;

    Ok(Json(MessageResponse {
        message: "user deleted".into(),
    }))
}

/// POST /admin/users/{id}/promote
///
/// Promotes a user to admin role. Returns 200 OK.
pub async fn promote_user_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    state.user_store.promote_user(user_id).await?;

    Ok(Json(MessageResponse {
        message: "user promoted to admin".into(),
    }))
}

/// POST /admin/users/{id}/demote
///
/// Demotes an admin to regular user. Returns 400 if this would leave zero admins.
pub async fn demote_user_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    state.user_store.demote_user(user_id).await?;

    Ok(Json(MessageResponse {
        message: "user demoted to regular user".into(),
    }))
}
