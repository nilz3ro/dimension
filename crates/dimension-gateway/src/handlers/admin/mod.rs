//! Admin management handlers for user and API key administration.
//!
//! All endpoints under `/admin/*` require the requesting user to be an admin
//! (enforced by `admin_middleware` via `route_layer`).

pub mod artifacts;
pub mod bundles;
pub mod health;
pub mod keys;
pub mod sessions;
pub mod storage;
pub mod tasks;
pub mod users;
pub mod workers;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Request types ─────────────────────────────────────────────────────────────

/// Body for POST /admin/users — create a new user.
#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub name: String,
}

/// Body for POST /admin/users/:id/keys — create an API key for a user.
#[derive(Debug, Deserialize)]
pub struct CreateKeyRequest {
    pub label: Option<String>,
}

// ── Response types ────────────────────────────────────────────────────────────

/// Response for POST /admin/users — new user + their first API key (shown once).
#[derive(Debug, Serialize)]
pub struct CreateUserResponse {
    pub user_id: Uuid,
    pub name: String,
    pub role: String,
    /// Plaintext API key. Shown once; never retrievable again.
    pub api_key: String,
}

/// A single user record in list/detail responses.
#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_max_sessions: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_max_bundles: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_max_concurrent_vms: Option<i32>,
}

/// Response for GET /admin/users.
#[derive(Debug, Serialize)]
pub struct UsersListResponse {
    pub users: Vec<UserResponse>,
}

/// Response for POST /admin/users/:id/keys — new API key (shown once).
#[derive(Debug, Serialize)]
pub struct CreateKeyResponse {
    pub key_id: Uuid,
    pub key_prefix: String,
    pub label: Option<String>,
    /// Plaintext API key. Shown once; never retrievable again.
    pub api_key: String,
}

/// Generic success message response.
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub message: String,
}
