//! Dimension gateway library.
#![allow(
    clippy::collapsible_if,
    clippy::unnecessary_map_or,
    clippy::needless_return,
    clippy::unnecessary_cast,
    clippy::let_and_return,
    clippy::unnecessary_get_then_check
)]
//!
//! This crate provides the host-side infrastructure for communicating
//! with guest agents running inside Firecracker microVMs. It includes:
//!
//! - HTTP server types: request models, error handling
//! - Backend abstraction: MessageHandler trait with mock echo implementation
//! - Bearer token auth middleware
//! - CLI configuration with clap derive
//! - Vsock connector with CONNECT handshake and exponential backoff retry
//! - Configuration types for connection and retry parameters
//! - Error types with rich diagnostics for debugging connection issues
//! - Webhook dispatch for session lifecycle events

pub mod worker;
pub mod bundle_security;
pub mod storage;
pub mod vault;
pub mod bundle_store;
pub mod config;
pub mod handlers;
pub mod lifecycle;
pub mod middleware;
pub mod models;
pub mod observability;
pub mod orchestration;
pub mod pulsar;
pub mod resilience;
pub mod server;

/// Shared test utilities (mock stores for tests that construct AppState).
/// Always compiled so integration tests can use them too.
pub mod test_utils;
