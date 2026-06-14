//! Deployment management HTTP handlers.
//!
//! - [`create_deployment_handler`]: POST /deployments — create a deployment record and start the VM
//! - [`stop_deployment_handler`]: DELETE /deployments/{id} — stop the VM and mark stopped
//! - [`list_deployments_handler`]: GET /deployments — list user's deployments with current status
//! - [`deployment_proxy_handler`]: /{deployment_id}/{*path} — reverse-proxy to deployment VM

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use axum::Json;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use dimension_store::{AuthenticatedUser, NewDeployment};

use crate::server::AppState;
use crate::worker::scheduler::pick_worker;
use crate::worker::worker_proto::{StartDeploymentRequest, StopDeploymentRequest};

/// POST /deployments request body.
#[derive(Debug, Deserialize)]
pub struct CreateDeploymentRequest {
    pub name: String,
    pub bundle_id: String,
    pub probe_port: Option<u32>, // default 8080
}

/// Deployment response item for GET /deployments and POST /deployments.
#[derive(Debug, Serialize)]
pub struct DeploymentResponse {
    pub id: Uuid,
    pub name: String,
    pub bundle_id: String,
    pub status: String,
    pub guest_ip: Option<String>,
    pub probe_port: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<dimension_store::Deployment> for DeploymentResponse {
    fn from(d: dimension_store::Deployment) -> Self {
        DeploymentResponse {
            id: d.id,
            name: d.name,
            bundle_id: d.bundle_id,
            status: d.status,
            guest_ip: d.guest_ip,
            probe_port: d.probe_port,
            created_at: d.created_at,
        }
    }
}

/// Validate a deployment name: 1-63 lowercase alphanumeric characters and hyphens.
fn validate_deployment_name(name: &str) -> bool {
    let re = Regex::new(r"^[a-z0-9-]{1,63}$").expect("valid regex");
    re.is_match(name)
}

/// POST /deployments — create a deployment record and start the VM on a worker.
///
/// Flow:
/// 1. Validate name format
/// 2. Create deployment record in 'starting' status
/// 3. Pick a worker via round-robin
/// 4. Call worker StartDeployment gRPC
/// 5. On success: update record with worker_id, guest_ip, pid and set status 'health_checking'
/// 6. On failure: set status 'stopped', return 502
pub async fn create_deployment_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Json(body): Json<CreateDeploymentRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    // Validate name
    if !validate_deployment_name(&body.name) {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

    let probe_port = body.probe_port.unwrap_or(8080) as i32;

    // Create deployment record (status starts as 'starting')
    let deployment = state
        .deployment_store
        .create_deployment(NewDeployment {
            user_id: user.user_id,
            bundle_id: body.bundle_id.clone(),
            name: body.name.clone(),
            probe_port,
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create deployment record");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let deployment_id = deployment.id;

    // Pick a worker — pick with no resource requirements for now (deployment uses defaults)
    let registry = match &state.worker_registry {
        Some(r) => r,
        None => {
            tracing::warn!("no worker registry — deployment requires multi-host mode");
            let _ = state
                .deployment_store
                .update_deployment_status(deployment_id, "stopped")
                .await;
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    };

    let worker = match pick_worker(registry, 0, 0) {
        Some(w) => w,
        None => {
            tracing::warn!("no available workers for deployment");
            let _ = state
                .deployment_store
                .update_deployment_status(deployment_id, "stopped")
                .await;
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    };

    let worker_id = worker.worker_id.to_string();
    let mut client = worker.client.clone();

    // Call worker StartDeployment gRPC
    let grpc_result = client
        .start_deployment(StartDeploymentRequest {
            deployment_id: deployment_id.to_string(),
            bundle_id: body.bundle_id.clone(),
            user_id: user.user_id.to_string(),
            probe_port: body.probe_port.unwrap_or(8080),
        })
        .await;

    match grpc_result {
        Ok(resp) => {
            let inner = resp.into_inner();
            if inner.success {
                // Record worker assignment, guest IP, and PID
                let pid = inner.pid as i32;
                if let Err(e) = state
                    .deployment_store
                    .update_deployment_worker(deployment_id, &worker_id, &inner.guest_ip, pid)
                    .await
                {
                    tracing::warn!(error = %e, %deployment_id, "failed to update deployment worker");
                }
                if let Err(e) = state
                    .deployment_store
                    .update_deployment_status(deployment_id, "health_checking")
                    .await
                {
                    tracing::warn!(error = %e, %deployment_id, "failed to set health_checking status");
                }

                // Re-fetch the updated deployment for the response
                let updated = state
                    .deployment_store
                    .get_deployment(deployment_id, user.user_id)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or(deployment);

                Ok((StatusCode::CREATED, Json(DeploymentResponse::from(updated))))
            } else {
                tracing::warn!(
                    %deployment_id,
                    error = %inner.error,
                    "worker failed to start deployment"
                );
                let _ = state
                    .deployment_store
                    .update_deployment_status(deployment_id, "stopped")
                    .await;
                Err(StatusCode::BAD_GATEWAY)
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, %deployment_id, "gRPC StartDeployment failed");
            let _ = state
                .deployment_store
                .update_deployment_status(deployment_id, "stopped")
                .await;
            Err(StatusCode::BAD_GATEWAY)
        }
    }
}

/// DELETE /deployments/{id} — stop the VM and mark the deployment stopped.
///
/// Idempotent: if already stopped, returns 200 immediately.
pub async fn stop_deployment_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    // Get deployment — scoped to user
    let deployment = state
        .deployment_store
        .get_deployment(id, user.user_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %id, "failed to get deployment");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Idempotent: already stopped
    if deployment.status == "stopped" {
        return Ok(axum::Json(serde_json::json!({ "message": "deployment stopped" })));
    }

    // Look up worker and call StopDeployment gRPC (best-effort)
    if let Some(ref worker_id) = deployment.worker_id {
        if let Some(ref registry) = state.worker_registry {
            // Find the worker by ID string
            let worker_uuid = worker_id.parse::<Uuid>().ok();
            let worker_opt = worker_uuid.and_then(|wid| registry.get(wid));

            if let Some(worker) = worker_opt {
                let mut client = worker.client.clone();
                if let Err(e) = client
                    .stop_deployment(StopDeploymentRequest {
                        deployment_id: id.to_string(),
                    })
                    .await
                {
                    tracing::warn!(
                        error = %e,
                        %id,
                        worker_id = %worker_id,
                        "StopDeployment gRPC failed (worker may be gone); continuing"
                    );
                }
            } else {
                tracing::warn!(
                    %id,
                    worker_id = %worker_id,
                    "worker not in registry for stop; marking stopped anyway"
                );
            }
        }
    }

    // Mark as stopped in store
    state
        .deployment_store
        .update_deployment_status(id, "stopped")
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %id, "failed to mark deployment stopped");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(%id, user_id = %user.user_id, "deployment stopped");

    Ok(axum::Json(serde_json::json!({ "message": "deployment stopped" })))
}

/// GET /deployments — list all deployments for the authenticated user.
pub async fn list_deployments_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, StatusCode> {
    let deployments = state
        .deployment_store
        .list_deployments(user.user_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to list deployments");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let response: Vec<DeploymentResponse> = deployments
        .into_iter()
        .map(DeploymentResponse::from)
        .collect();

    Ok(axum::Json(serde_json::json!({ "deployments": response })))
}

/// /{deployment_id}/{*path} — reverse-proxy to the deployment VM HTTP listener.
///
/// No auth required: the deployment URL is a public HTTP service.
/// Returns 503 if the deployment is not healthy or health_checking.
/// Returns 404 if the deployment does not exist or the ID is not a valid UUID.
/// Returns 502 if the upstream request fails.
pub async fn deployment_proxy_handler(
    State(state): State<AppState>,
    Path((deployment_id_str, rest_path)): Path<(String, String)>,
    mut req: Request<Body>,
) -> Result<Response, StatusCode> {
    // Parse UUID — if invalid, this URL is not a deployment proxy path
    let deployment_id = deployment_id_str
        .parse::<Uuid>()
        .map_err(|_| StatusCode::NOT_FOUND)?;

    // Look up the deployment — no user scoping (public proxy URL)
    let deployment = state
        .deployment_store
        .get_deployment_public(deployment_id)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, %deployment_id, "proxy: deployment store error");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    if deployment.status != "healthy" && deployment.status != "health_checking" {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let guest_ip = deployment.guest_ip.ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let port = deployment.probe_port;

    // Build upstream URI: http://{guest_ip}:{port}/{rest_path}?{query}
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let upstream_uri = format!("http://{guest_ip}:{port}/{rest_path}{query}")
        .parse::<hyper::Uri>()
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    *req.uri_mut() = upstream_uri;

    // Remove hop-by-hop headers
    let headers = req.headers_mut();
    headers.remove("host");
    headers.remove("connection");
    headers.remove("transfer-encoding");

    let client: Client<HttpConnector, Body> =
        Client::builder(hyper_util::rt::TokioExecutor::new()).build(HttpConnector::new());

    client
        .request(req)
        .await
        .map(|resp| resp.into_response())
        .map_err(|e| {
            tracing::warn!(error = %e, %deployment_id, "proxy request failed");
            StatusCode::BAD_GATEWAY
        })
}
