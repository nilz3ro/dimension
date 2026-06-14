//! Admin role guard middleware.
//!
//! Reads `AuthenticatedUser` from request extensions (already set by `auth_middleware`)
//! and rejects non-admin users with 403 Forbidden.
//!
//! # Usage
//!
//! MUST be applied with `route_layer` (not `layer`) on the admin sub-router.
//! Using `layer` would intercept requests to non-existent admin paths and return
//! 403 instead of 404, leaking the existence of the `/admin` prefix.
//!
//! ```text
//! let admin = Router::new()
//!     .route("/users", post(create_user_handler))
//!     .route_layer(axum::middleware::from_fn(admin_middleware));
//! ```

use axum::{extract::Request, middleware::Next, response::Response, Extension};
use dimension_store::{AuthenticatedUser, UserRole};

use crate::models::error::AppError;

/// Admin role guard for use with `route_layer(from_fn(admin_middleware))`.
///
/// Reads `AuthenticatedUser` from extensions (injected by `auth_middleware`) and
/// returns 403 Forbidden if the user is not an admin. If the extension is missing
/// (which should never happen if layering is correct), axum returns 500.
pub async fn admin_middleware(
    Extension(user): Extension<AuthenticatedUser>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    if user.role != UserRole::Admin {
        return Err(AppError::Forbidden("admin access required".into()));
    }
    Ok(next.run(request).await)
}
