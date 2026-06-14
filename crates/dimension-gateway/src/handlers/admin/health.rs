//! Admin full health check handler.
//!
//! GET /admin/health — comprehensive platform health check covering:
//! Postgres, object storage, Vault, and worker registry.

use axum::extract::{Extension, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

use dimension_store::AuthenticatedUser;

use crate::server::AppState;
use crate::vault::VaultHealthStatus;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Status for a single service in the health response.
#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub name: &'static str,
    /// "ok", "degraded", "not_configured", or "configured".
    pub status: String,
    /// Optional human-readable detail (for degraded status or worker counts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Response for GET /admin/health.
#[derive(Debug, Serialize)]
pub struct AdminHealthResponse {
    /// "ok" if all configured services are healthy; "degraded" if any are not.
    pub status: String,
    pub version: &'static str,
    pub uptime_secs: u64,
    pub services: Vec<ServiceStatus>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// GET /admin/health — full platform health check.
///
/// Checks each configured service and returns a comprehensive status overview.
/// Overall status is "degraded" if any configured service reports "degraded".
pub async fn admin_health_handler(
    Extension(_admin): Extension<AuthenticatedUser>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let mut services = Vec::new();
    let mut any_degraded = false;

    // 1. Postgres: attempt a lightweight query via user_store.admin_count().
    let postgres_status = match state.user_store.admin_count().await {
        Ok(_) => ServiceStatus {
            name: "postgres",
            status: "ok".into(),
            detail: None,
        },
        Err(e) => {
            any_degraded = true;
            ServiceStatus {
                name: "postgres",
                status: "degraded".into(),
                detail: Some(e.to_string()),
            }
        }
    };
    services.push(postgres_status);

    // 2. Object storage: presence check (no actual I/O for health check).
    let storage_status = match &state.storage_client {
        None => ServiceStatus {
            name: "object_storage",
            status: "not_configured".into(),
            detail: None,
        },
        Some(_) => ServiceStatus {
            name: "object_storage",
            status: "configured".into(),
            detail: None,
        },
    };
    services.push(storage_status);

    // 3. Vault: check_health() call.
    let vault_status = match &state.vault_client {
        None => ServiceStatus {
            name: "vault",
            status: "not_configured".into(),
            detail: None,
        },
        Some(client) => match client.check_health().await {
            VaultHealthStatus::Ok => ServiceStatus {
                name: "vault",
                status: "ok".into(),
                detail: None,
            },
            VaultHealthStatus::Degraded(reason) => {
                any_degraded = true;
                ServiceStatus {
                    name: "vault",
                    status: "degraded".into(),
                    detail: Some(reason),
                }
            }
        },
    };
    services.push(vault_status);

    // 4. Workers: count healthy vs draining.
    let workers_status = match &state.worker_registry {
        None => ServiceStatus {
            name: "workers",
            status: "not_configured".into(),
            detail: Some("single-host mode".into()),
        },
        Some(registry) => {
            let all = registry.get_all();
            let total = all.len();
            let draining = all.iter().filter(|w| w.draining).count();
            let healthy = total - draining;
            ServiceStatus {
                name: "workers",
                status: if total == 0 { "degraded".to_string() } else { "ok".to_string() },
                detail: Some(format!("{total} registered, {draining} draining, {healthy} healthy")),
            }
        }
    };
    services.push(workers_status);

    let overall = if any_degraded { "degraded" } else { "ok" };

    Json(AdminHealthResponse {
        status: overall.into(),
        version: VERSION,
        uptime_secs: state.startup_time.elapsed().as_secs(),
        services,
    })
}
