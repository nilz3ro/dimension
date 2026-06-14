//! Middleware components for the HTTP server.
//!
//! Provides authentication, concurrency limiting, and other
//! cross-cutting concerns as axum-compatible middleware functions.

pub mod admin;
pub mod auth;
pub mod concurrency;

pub use admin::admin_middleware;
pub use auth::auth_middleware;
pub use concurrency::concurrency_limit_middleware;
