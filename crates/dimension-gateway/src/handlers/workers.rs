//! Worker registration HTTP endpoint.
//!
//! POST /internal/workers/register — called by worker nodes at startup.
//! This endpoint is NOT auth-gated (internal network only).

use std::time::Instant;

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use crate::models::error::AppError;
use crate::server::AppState;
use crate::worker::registry::WorkerState;
use crate::worker::worker_proto::worker_service_client::WorkerServiceClient;

/// Capacity reported by the worker at registration.
#[derive(Debug, Deserialize)]
pub struct WorkerCapacity {
    pub memory_mb: u64,
    pub vcpus: u32,
}

/// Worker registration request body.
#[derive(Debug, Deserialize)]
pub struct RegisterWorkerRequest {
    pub worker_id: String,
    pub grpc_addr: String,
    pub capacity: WorkerCapacity,
}

/// Response body for successful registration.
#[derive(Debug, Serialize)]
pub struct RegisterWorkerResponse {
    pub accepted: bool,
}

/// POST /internal/workers/register
///
/// Accepts worker registration. Creates a gRPC connection to the worker and
/// stores it in the WorkerRegistry. Returns 400 if the worker is unreachable.
pub async fn register_worker_handler(
    State(state): State<AppState>,
    Json(body): Json<RegisterWorkerRequest>,
) -> Result<impl IntoResponse, AppError> {
    let worker_id = Uuid::parse_str(&body.worker_id).map_err(|_| {
        AppError::BadRequest(format!("invalid worker_id UUID: {}", body.worker_id))
    })?;

    // Validate grpc_addr format (must be a valid endpoint URL).
    let grpc_addr = if body.grpc_addr.starts_with("http://") || body.grpc_addr.starts_with("https://") {
        body.grpc_addr.clone()
    } else {
        // Assume plain host:port — add http:// prefix.
        format!("http://{}", body.grpc_addr)
    };

    // Establish gRPC connection to the worker.
    let channel = tonic::transport::Endpoint::from_shared(grpc_addr.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid grpc_addr '{}': {e}", body.grpc_addr)))?
        .connect()
        .await
        .map_err(|e| {
            warn!(
                grpc_addr = %grpc_addr,
                error = %e,
                "failed to connect to worker gRPC endpoint"
            );
            AppError::BadRequest(format!(
                "cannot reach worker at {}: {e}",
                body.grpc_addr
            ))
        })?;

    let client = WorkerServiceClient::new(channel)
        .max_decoding_message_size(1024 * 1024 * 1024)   // 1 GiB
        .max_encoding_message_size(1024 * 1024 * 1024);   // 1 GiB

    let state_entry = WorkerState {
        worker_id,
        grpc_addr: grpc_addr.clone(),
        available_memory_mb: body.capacity.memory_mb,
        available_vcpus: body.capacity.vcpus,
        running_vms: 0,
        draining: false,
        last_seen: Instant::now(),
        last_heartbeat: Some(Instant::now()),
        client,
    };

    if let Some(ref registry) = state.worker_registry {
        registry.register(state_entry);
    }

    info!(
        worker_id = %worker_id,
        grpc_addr = %grpc_addr,
        memory_mb = body.capacity.memory_mb,
        vcpus = body.capacity.vcpus,
        "worker registered"
    );

    Ok((StatusCode::OK, Json(RegisterWorkerResponse { accepted: true })))
}
