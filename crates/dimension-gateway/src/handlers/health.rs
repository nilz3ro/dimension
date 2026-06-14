//! Health check handler.
//!
//! Returns a JSON response with gateway status, version, uptime, active VM count,
//! and Vault status. This endpoint is not protected by authentication middleware.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::server::AppState;
use crate::vault::VaultHealthStatus;

/// Gateway version from Cargo.toml, resolved at compile time.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Health check response body.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// "ok" when reachable; "degraded" when Vault is unhealthy.
    pub status: &'static str,
    /// Gateway version from Cargo.toml.
    pub version: &'static str,
    /// Seconds since the gateway started accepting traffic.
    pub uptime_secs: u64,
    /// Number of VMs currently running (in-flight requests).
    pub active_vms: usize,
    /// Vault connectivity status: "ok", "degraded", or "not_configured".
    pub vault_status: String,
    /// Optional detail message when vault_status is "degraded".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault_detail: Option<String>,
}

/// GET /health handler.
///
/// Returns gateway health status with version, uptime, active VM count, and Vault status.
/// No authentication required.
///
/// Per design decision: overall status stays "ok" even when Vault is degraded --
/// the platform still runs for bundles that don't use secrets. When Vault is
/// unhealthy, status is set to "degraded" to signal ops monitoring.
pub async fn health_handler(
    State(state): State<AppState>,
) -> Json<HealthResponse> {
    let (vault_status, vault_detail, overall_status) = match &state.vault_client {
        None => ("not_configured".to_string(), None, "ok"),
        Some(client) => match client.check_health().await {
            VaultHealthStatus::Ok => ("ok".to_string(), None, "ok"),
            VaultHealthStatus::Degraded(reason) => {
                ("degraded".to_string(), Some(reason), "degraded")
            }
        },
    };

    Json(HealthResponse {
        status: overall_status,
        version: VERSION,
        uptime_secs: state.startup_time.elapsed().as_secs(),
        active_vms: state.concurrency_controller.active(),
        vault_status,
        vault_detail,
    })
}
