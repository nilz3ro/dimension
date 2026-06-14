//! Run invocation HTTP handlers.
//!
//! - [`run_handler`]: POST /run — dispatch a bundle invocation to a worker
//! - [`stop_handler`]: POST /run/{id}/stop — stop a running invocation

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use dimension_store::AuthenticatedUser;
use hyphae_core::registry::Registry;

use crate::models::error::AppError;
use crate::server::AppState;
use crate::worker::scheduler::pick_worker;
use crate::worker::worker_proto::{RunInvocationRequest, StopInvocationRequest};

/// POST /run request body.
#[derive(Debug, Deserialize)]
pub struct RunRequest {
    /// Bundle name or numeric ID to run.
    pub bundle_id: String,
    /// Execution mode: "sync" or "async".
    pub mode: String,
    /// JSON payload forwarded to the VM via MMDS.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Host-side vsock port for stdout capture (sync mode).
    /// Defaults to 1024 if not specified.
    pub vsock_port: Option<u32>,
    /// Per-invocation timeout in seconds (0 or absent = server default).
    pub timeout_secs: Option<u32>,
}

/// POST /run response body (sync mode — 200).
#[derive(Debug, Serialize)]
pub struct RunSyncResponse {
    pub invocation_id: String,
    pub stdout: String,
    pub exit_code: i32,
    /// Path to the Server-Sent Events stream for this run. Clients can
    /// connect here to replay events from sequence 0 or resume from a
    /// specific sequence with the `Last-Event-ID` header.
    pub events_url: String,
}

/// POST /run response body (async mode — 202).
#[derive(Debug, Serialize)]
pub struct RunAsyncResponse {
    pub invocation_id: String,
    pub status: &'static str,
    pub events_url: String,
}

/// Validate the mode string. Returns an error if invalid.
fn validate_mode(mode: &str) -> Result<(), AppError> {
    match mode {
        "sync" | "async" => Ok(()),
        _ => Err(AppError::BadRequest(format!(
            "invalid mode '{}': must be 'sync' or 'async'",
            mode
        ))),
    }
}

/// POST /run — dispatch a bundle invocation to a worker.
///
/// Flow:
/// 1. Validate request fields (bundle_id, mode, payload)
/// 2. Resolve the bundle from the registry (verify it exists)
/// 3. Pick a worker via round-robin scheduler
/// 4. Call RunInvocation gRPC on the selected worker
/// 5. For sync mode: return 200 with stdout and exit_code
/// 6. For async mode: return 202 with invocation_id
pub async fn run_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Json(body): Json<RunRequest>,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;

    // Validate required fields
    if body.bundle_id.is_empty() {
        return Err(AppError::BadRequest("bundle_id is required".into()));
    }
    validate_mode(&body.mode)?;

    // Serialize the payload (even if null/empty, worker expects bytes)
    let payload_bytes = serde_json::to_vec(&body.payload)
        .map_err(|e| AppError::BadRequest(format!("failed to serialize payload: {e}")))?;

    if payload_bytes.is_empty() {
        return Err(AppError::BadRequest("payload is required".into()));
    }

    // Resolve the bundle from the local registry to verify it exists.
    let registry_path = state.registry_path.clone();
    let bundle_id_str = body.bundle_id.clone();
    let user_id_str = user.user_id.to_string();
    let is_admin = user.role == dimension_store::UserRole::Admin;

    tokio::task::spawn_blocking(move || -> Result<(), AppError> {
        let registry = Registry::open(&registry_path)
            .map_err(|e| AppError::Internal(Box::new(e)))?;

        // Try numeric ID first, then name lookup
        let found = if let Ok(id) = bundle_id_str.parse::<i64>() {
            registry
                .find_by_id(id)
                .map_err(|e| AppError::Internal(Box::new(e)))?
        } else {
            // Look up by name+tag (latest)
            registry
                .find_by_name_tag(&bundle_id_str, "latest")
                .map_err(|e| AppError::Internal(Box::new(e)))?
        };

        match found {
            Some(img) => {
                // Check ownership: platform bundles visible to all, user bundles only to owner/admin
                if !is_admin {
                    if let Some(ref owner) = img.owner_id {
                        if *owner != user_id_str {
                            return Err(AppError::NotFound("bundle not found".into()));
                        }
                    }
                }
                Ok(())
            }
            None => Err(AppError::NotFound(format!(
                "bundle '{}' not found",
                bundle_id_str
            ))),
        }
    })
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))??;

    // Pick a worker
    let registry = state
        .worker_registry
        .as_ref()
        .ok_or_else(|| {
            AppError::ServiceUnavailable("no worker registry configured (multi-host mode required)".into())
        })?;

    let worker = pick_worker(registry, 0, 0).ok_or_else(|| {
        AppError::ServiceUnavailable("no workers available to handle invocation".into())
    })?;

    let mut client = worker.client.clone();
    let vsock_port = body.vsock_port.unwrap_or(1024);
    let timeout_secs = body.timeout_secs.unwrap_or(0);

    // Call RunInvocation gRPC
    let grpc_request = tonic::Request::new(RunInvocationRequest {
        bundle_id: body.bundle_id.clone(),
        mode: body.mode.clone(),
        payload: payload_bytes,
        vsock_port,
        timeout_secs,
        user_id: user.user_id.to_string(),
    });

    let grpc_result = client.run_invocation(grpc_request).await;

    match grpc_result {
        Ok(resp) => {
            let inner = resp.into_inner();
            if !inner.success {
                // Worker returned an error
                tracing::warn!(
                    bundle_id = %body.bundle_id,
                    mode = %body.mode,
                    error = %inner.error,
                    "RunInvocation failed on worker"
                );
                return Err(AppError::Internal(Box::new(std::io::Error::other(
                    inner.error,
                ))));
            }

            let events_url = format!("/runs/{}/events", inner.invocation_id);
            match body.mode.as_str() {
                "sync" => {
                    let stdout = String::from_utf8_lossy(&inner.stdout).to_string();
                    Ok((
                        StatusCode::OK,
                        Json(RunSyncResponse {
                            invocation_id: inner.invocation_id,
                            stdout,
                            exit_code: inner.exit_code,
                            events_url,
                        }),
                    )
                        .into_response())
                }
                "async" => Ok((
                    StatusCode::ACCEPTED,
                    Json(RunAsyncResponse {
                        invocation_id: inner.invocation_id,
                        status: "started",
                        events_url,
                    }),
                )
                    .into_response()),
                _ => unreachable!("mode already validated"),
            }
        }
        Err(status) => {
            tracing::warn!(
                bundle_id = %body.bundle_id,
                mode = %body.mode,
                error = %status,
                "RunInvocation gRPC call failed"
            );

            // Map gRPC status codes to HTTP status codes per failure modes table
            match status.code() {
                tonic::Code::DeadlineExceeded => Err(AppError::GatewayTimeout(
                    "worker timed out processing invocation".into(),
                )),
                tonic::Code::Unavailable => Err(AppError::ServiceUnavailable(
                    "worker unavailable".into(),
                )),
                tonic::Code::NotFound => Err(AppError::NotFound(
                    status.message().to_string(),
                )),
                tonic::Code::InvalidArgument => Err(AppError::BadRequest(
                    status.message().to_string(),
                )),
                _ => Err(AppError::Internal(Box::new(std::io::Error::other(
                    format!("gRPC error: {status}"),
                )))),
            }
        }
    }
}

/// POST /run/{id}/stop — stop a running invocation.
///
/// Finds the invocation's worker and calls StopInvocation gRPC.
/// For now, broadcasts to all workers since we don't track invocation-to-worker mapping
/// at the gateway level (that's T02's job with invocation state tracking).
pub async fn stop_handler(
    State(state): State<AppState>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(invocation_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Validate the invocation_id is a valid UUID
    let _id = invocation_id.parse::<Uuid>().map_err(|_| {
        AppError::BadRequest(format!("invalid invocation_id: '{invocation_id}'"))
    })?;

    let registry = state
        .worker_registry
        .as_ref()
        .ok_or_else(|| {
            AppError::ServiceUnavailable("no worker registry configured".into())
        })?;

    let workers = registry.get_all();
    if workers.is_empty() {
        return Err(AppError::ServiceUnavailable(
            "no workers available".into(),
        ));
    }

    // Broadcast StopInvocation to all workers — the one owning the invocation
    // will stop it, others will return NOT_FOUND which we ignore.
    let mut found = false;
    let mut last_error: Option<String> = None;

    for worker in workers {
        let mut client = worker.client.clone();
        let grpc_request = tonic::Request::new(StopInvocationRequest {
            invocation_id: invocation_id.clone(),
        });

        match client.stop_invocation(grpc_request).await {
            Ok(resp) => {
                let inner = resp.into_inner();
                if inner.success {
                    found = true;
                    tracing::info!(
                        invocation_id = %invocation_id,
                        worker_id = %worker.worker_id,
                        "invocation stopped"
                    );
                    break;
                }
                // Worker didn't own this invocation — continue
            }
            Err(status) => {
                if status.code() == tonic::Code::NotFound {
                    // Expected — this worker doesn't own the invocation
                    continue;
                }
                if status.code() == tonic::Code::DeadlineExceeded {
                    return Err(AppError::GatewayTimeout(
                        "worker timed out stopping invocation".into(),
                    ));
                }
                tracing::warn!(
                    invocation_id = %invocation_id,
                    worker_id = %worker.worker_id,
                    error = %status,
                    "StopInvocation gRPC error"
                );
                last_error = Some(format!("gRPC error from worker {}: {status}", worker.worker_id));
            }
        }
    }

    if found {
        Ok(Json(serde_json::json!({
            "message": "invocation stopped",
            "invocation_id": invocation_id,
        })))
    } else if let Some(err) = last_error {
        Err(AppError::Internal(Box::new(std::io::Error::other(
            err,
        ))))
    } else {
        Err(AppError::NotFound(format!(
            "invocation '{}' not found on any worker",
            invocation_id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use dimension_store::{AuthenticatedUser, UserRole};
    use hyphae_core::registry::Registry;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::bundle_store::BundleJobStore;
    use crate::config::AppConfig;
    use crate::resilience::ConcurrencyController;
    use crate::server::AppState;

    // ── Noop stores (same pattern as bundles.rs tests) ────────────────────

    struct NoopUserStore;
    #[async_trait::async_trait]
    impl dimension_store::UserStore for NoopUserStore {
        async fn create_user(&self, _n: &str, _r: UserRole) -> Result<(dimension_store::User, String), dimension_store::StoreError> { unimplemented!() }
        async fn get_user(&self, _id: Uuid) -> Result<Option<dimension_store::User>, dimension_store::StoreError> { unimplemented!() }
        async fn list_users(&self) -> Result<Vec<dimension_store::User>, dimension_store::StoreError> { unimplemented!() }
        async fn soft_delete_user(&self, _: Uuid) -> Result<(), dimension_store::StoreError> { unimplemented!() }
        async fn promote_user(&self, _: Uuid) -> Result<(), dimension_store::StoreError> { unimplemented!() }
        async fn demote_user(&self, _: Uuid) -> Result<(), dimension_store::StoreError> { unimplemented!() }
        async fn admin_count(&self) -> Result<i64, dimension_store::StoreError> { unimplemented!() }
        async fn create_key(&self, _: Uuid, _: Option<&str>) -> Result<(dimension_store::ApiKey, String), dimension_store::StoreError> { unimplemented!() }
        async fn authenticate_key(&self, _: &str) -> Result<AuthenticatedUser, dimension_store::StoreError> { unimplemented!() }
        async fn revoke_key(&self, _: Uuid) -> Result<(), dimension_store::StoreError> { unimplemented!() }
        async fn list_keys_for_user(&self, _: Uuid) -> Result<Vec<dimension_store::ApiKey>, dimension_store::StoreError> { Ok(vec![]) }
        async fn ensure_bootstrap_admin(&self) -> Result<Option<String>, dimension_store::StoreError> { unimplemented!() }
        async fn get_bootstrap_admin(&self) -> Result<AuthenticatedUser, dimension_store::StoreError> { unimplemented!() }
    }

    struct NoopSecretStore;
    #[async_trait::async_trait]
    impl dimension_store::SecretStore for NoopSecretStore {
        async fn upsert_secret_metadata(&self, _u: Uuid, _b: &str, _n: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn list_secret_metadata(&self, _u: Uuid, _b: &str) -> Result<Vec<dimension_store::SecretMetadata>, dimension_store::StoreError> { Ok(vec![]) }
        async fn delete_secret_metadata(&self, _u: Uuid, _b: &str, _n: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn insert_token(&self, _id: &str, _bid: &str, _u: Uuid, _c: &str, _e: Option<chrono::DateTime<chrono::Utc>>) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn get_token(&self, _id: &str, _b: &str) -> Result<Option<dimension_store::TokenRecord>, dimension_store::StoreError> { Ok(None) }
        async fn delete_expired_tokens(&self) -> Result<u64, dimension_store::StoreError> { Ok(0) }
    }

    struct NoopStorageStore;
    #[async_trait::async_trait]
    impl dimension_store::StorageStore for NoopStorageStore {
        async fn get_storage_info(&self, _u: Uuid, _b: &str) -> Result<(i64, i64), dimension_store::StoreError> { Ok((0, 104857600)) }
        async fn increment_bytes_used(&self, _u: Uuid, _b: &str, _d: i64) -> Result<i64, dimension_store::StoreError> { Ok(0) }
        async fn decrement_bytes_used(&self, _u: Uuid, _b: &str, _d: i64) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn set_quota(&self, _u: Uuid, _b: &str, _q: i64) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn list_storage_stats(&self) -> Result<Vec<dimension_store::BundleStorageRecord>, dimension_store::StoreError> { Ok(vec![]) }
        async fn set_bytes_used(&self, _u: Uuid, _b: &str, _bytes: i64) -> Result<(), dimension_store::StoreError> { Ok(()) }
    }

    struct NoopTaskStore;
    #[async_trait::async_trait]
    impl dimension_store::TaskStore for NoopTaskStore {
        async fn upsert_agent_card(&self, _b: &str, _u: Uuid, _j: serde_json::Value) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn get_agent_card(&self, _b: &str) -> Result<Option<serde_json::Value>, dimension_store::StoreError> { Ok(None) }
        async fn delete_agent_card(&self, _b: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn get_or_create_agent_session(&self, _c: &str, _t: &str, _u: Uuid, _ss: &dyn dimension_store::SessionStore) -> Result<Uuid, dimension_store::StoreError> { Ok(Uuid::new_v4()) }
        async fn create_task(&self, _t: dimension_store::NewTask) -> Result<dimension_store::Task, dimension_store::StoreError> { unimplemented!() }
        async fn get_task(&self, _id: Uuid, _u: Uuid) -> Result<Option<dimension_store::Task>, dimension_store::StoreError> { Ok(None) }
        async fn get_task_scoped(&self, _id: Uuid, _u: Uuid, _b: &str) -> Result<Option<dimension_store::Task>, dimension_store::StoreError> { Ok(None) }
        async fn list_tasks(&self, _u: Uuid, _s: Option<&str>, _c: Option<&str>, _l: i64) -> Result<(Vec<dimension_store::Task>, Option<String>), dimension_store::StoreError> { Ok((vec![], None)) }
        async fn update_task_status(&self, _id: Uuid, _s: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn cancel_task(&self, _id: Uuid, _u: Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn claim_ready_tasks(&self, _l: i32) -> Result<Vec<dimension_store::Task>, dimension_store::StoreError> { Ok(vec![]) }
        async fn complete_task_iteration(&self, _id: Uuid, _r: dimension_store::NewTaskRun) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn get_task_runs(&self, _id: Uuid, _u: Uuid) -> Result<Vec<dimension_store::TaskRun>, dimension_store::StoreError> { Ok(vec![]) }
        async fn count_running_tasks(&self, _u: Uuid) -> Result<i64, dimension_store::StoreError> { Ok(0) }
        async fn get_task_targets(&self, _id: Uuid) -> Result<Vec<String>, dimension_store::StoreError> { Ok(vec![]) }
        async fn set_task_next_run(&self, _id: Uuid, _t: chrono::DateTime<chrono::Utc>) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn admin_list_tasks(&self, _s: Option<&str>, _c: Option<&str>, _l: i64) -> Result<(Vec<dimension_store::Task>, Option<String>), dimension_store::StoreError> { Ok((vec![], None)) }
        async fn admin_get_task(&self, _id: Uuid) -> Result<Option<dimension_store::Task>, dimension_store::StoreError> { Ok(None) }
        async fn retry_task(&self, _id: Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
    }

    struct NoopVolumeStore;
    #[async_trait::async_trait]
    impl dimension_store::VolumeStore for NoopVolumeStore {
        async fn create_volume(&self, _u: Uuid, _s: i64) -> Result<dimension_store::Volume, dimension_store::StoreError> { unimplemented!() }
        async fn get_volume(&self, _id: Uuid) -> Result<Option<dimension_store::Volume>, dimension_store::StoreError> { Ok(None) }
        async fn list_volumes_for_user(&self, _u: Uuid) -> Result<Vec<dimension_store::Volume>, dimension_store::StoreError> { Ok(vec![]) }
        async fn attach_volume(&self, _vid: Uuid, _sid: Uuid) -> Result<dimension_store::Volume, dimension_store::StoreError> { unimplemented!() }
        async fn detach_volume(&self, _vid: Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn set_worker(&self, _vid: Uuid, _w: Option<&str>) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn delete_volume(&self, _vid: Uuid, _uid: Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn find_volume_for_session(&self, _sid: Uuid) -> Result<Option<dimension_store::Volume>, dimension_store::StoreError> { Ok(None) }
        async fn touch_volume(&self, _vid: Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
    }

    struct NoopArtifactStore;
    #[async_trait::async_trait]
    impl dimension_store::ArtifactStore for NoopArtifactStore {
        async fn put_artifact(&self, _op: &opendal::Operator, _sid: Uuid, _uid: Uuid, _key: &str, _data: bytes::Bytes, _ct: Option<&str>) -> Result<dimension_store::Artifact, dimension_store::StoreError> { unimplemented!() }
        async fn list_artifacts_for_session(&self, _sid: Uuid, _uid: Uuid) -> Result<Vec<dimension_store::Artifact>, dimension_store::StoreError> { Ok(vec![]) }
        async fn get_artifact(&self, _sid: Uuid, _uid: Uuid, _key: &str) -> Result<Option<dimension_store::Artifact>, dimension_store::StoreError> { Ok(None) }
        async fn delete_artifact(&self, _op: &opendal::Operator, _sid: Uuid, _uid: Uuid, _key: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn list_artifacts_for_user(&self, _uid: Uuid, _cursor: Option<&str>, _limit: i64) -> Result<(Vec<dimension_store::Artifact>, Option<String>), dimension_store::StoreError> { Ok((vec![], None)) }
        async fn admin_list_artifacts(&self, _cursor: Option<&str>, _limit: i64) -> Result<(Vec<dimension_store::Artifact>, Option<String>), dimension_store::StoreError> { Ok((vec![], None)) }
        async fn delete_expired_artifacts(&self, _op: &opendal::Operator) -> Result<u64, dimension_store::StoreError> { Ok(0) }
    }

    struct NoopDeploymentStore;
    #[async_trait::async_trait]
    impl dimension_store::DeploymentStore for NoopDeploymentStore {
        async fn create_deployment(&self, _n: dimension_store::NewDeployment) -> Result<dimension_store::Deployment, dimension_store::StoreError> { unimplemented!() }
        async fn get_deployment(&self, _id: Uuid, _u: Uuid) -> Result<Option<dimension_store::Deployment>, dimension_store::StoreError> { Ok(None) }
        async fn list_deployments(&self, _u: Uuid) -> Result<Vec<dimension_store::Deployment>, dimension_store::StoreError> { Ok(vec![]) }
        async fn update_deployment_status(&self, _id: Uuid, _s: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn update_deployment_worker(&self, _id: Uuid, _w: &str, _ip: &str, _pid: i32) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn increment_probe_failures(&self, _id: Uuid) -> Result<i32, dimension_store::StoreError> { Ok(0) }
        async fn reset_probe_failures(&self, _id: Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn list_active_on_worker(&self, _w: &str) -> Result<Vec<dimension_store::Deployment>, dimension_store::StoreError> { Ok(vec![]) }
        async fn mark_orphaned_for_worker(&self, _w: &str) -> Result<u64, dimension_store::StoreError> { Ok(0) }
        async fn get_deployment_public(&self, _id: Uuid) -> Result<Option<dimension_store::Deployment>, dimension_store::StoreError> { Ok(None) }
        async fn list_active_worker_ids(&self) -> Result<Vec<String>, dimension_store::StoreError> { Ok(vec![]) }
        async fn list_probeable_deployments(&self) -> Result<Vec<dimension_store::Deployment>, dimension_store::StoreError> { Ok(vec![]) }
    }

    struct NoopNamedVolumeStore;
    #[async_trait::async_trait]
    impl dimension_store::NamedVolumeStore for NoopNamedVolumeStore {
        async fn create_named_volume(&self, _u: Uuid, _n: &str, _s: i64) -> Result<dimension_store::NamedVolume, dimension_store::StoreError> { unimplemented!() }
        async fn get_named_volume(&self, _id: Uuid) -> Result<Option<dimension_store::NamedVolume>, dimension_store::StoreError> { Ok(None) }
        async fn find_named_volume_by_name(&self, _u: Uuid, _n: &str) -> Result<Option<dimension_store::NamedVolume>, dimension_store::StoreError> { Ok(None) }
        async fn list_named_volumes_for_user(&self, _u: Uuid) -> Result<Vec<dimension_store::NamedVolume>, dimension_store::StoreError> { Ok(vec![]) }
        async fn set_named_volume_worker(&self, _id: Uuid, _w: Option<&str>) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn delete_named_volume(&self, _id: Uuid, _u: Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
    }

    fn make_test_state(registry_path: std::path::PathBuf) -> AppState {
        let config = AppConfig {
            port: 3000,
            host: "127.0.0.1".into(),
            token: "test-token".into(),
            kernel_path: "/opt/hyphae/kernel/vmlinux".into(),
            firecracker_bin: "firecracker".into(),
            boot_timeout_secs: 30,
            processing_timeout_secs: 300,
            max_boot_timeout_secs: 60,
            max_processing_timeout_secs: 600,
            registry_path: Some(registry_path.clone()),
            mock: true,
            enable_network: false,
            max_concurrent: 200,
            heartbeat_interval_secs: 15,
            max_vcpus: 8,
            max_memory_mib: 8192,
            max_disk_size_mib: 65536,
            drain_timeout_secs: 60,
            database_url: "postgres://unused/in-tests".into(),
            vault: crate::vault::VaultConfig {
                vault_url: "http://127.0.0.1:8200".into(),
                vault_role_id: None,
                vault_secret_id: None,
                vault_renewal_interval_secs: 900,
                vault_vm_token_ttl_secs: 3600,
            },
            storage: crate::storage::StorageConfig {
                endpoint: "http://127.0.0.1:9000".into(),
                bucket: "dimension".into(),
                access_key: None,
                secret_key: None,
            },
            max_concurrent_tasks_per_user: 5,
            multi_host: false,
            worker_health_interval_secs: 15,
            clickhouse_url: "http://localhost:8123".into(),
            clickhouse_database: "dimension".into(),
            log_minio_endpoint: "http://127.0.0.1:9000".into(),
            log_minio_bucket: "dimension-logs".into(),
            log_minio_access_key: None,
            log_minio_secret_key: None,
            pulsar_url: None,
            pulsar_topic: "persistent://dimension/events/runs".into(),
        };
        AppState {
            concurrency_controller: Arc::new(ConcurrencyController::new(200)),
            resource_caps: config.resource_caps(),
            user_store: Arc::new(NoopUserStore),
            session_store: Arc::new(crate::test_utils::MockSessionStore::new()),
            bundle_job_store: BundleJobStore::new(),
            registry_path,
            config,
            startup_time: Instant::now(),
            drain_token: CancellationToken::new(),
            vault_client: None,
            secret_store: Arc::new(NoopSecretStore),
            storage_client: None,
            storage_store: Arc::new(NoopStorageStore),
            task_store: Arc::new(NoopTaskStore),
            worker_registry: None,
            volume_store: Arc::new(NoopVolumeStore),
            artifact_store: Arc::new(NoopArtifactStore),
            deployment_store: Arc::new(NoopDeploymentStore),
            named_volume_store: Arc::new(NoopNamedVolumeStore),
            log_broadcaster: crate::observability::LogBroadcaster::new(),
            http_client: reqwest::Client::new(),
            clickhouse_client: None,
            log_storage_client: None,
            pulsar_client: None,
        }
    }

    fn make_test_user() -> AuthenticatedUser {
        AuthenticatedUser {
            user_id: Uuid::new_v4(),
            name: "test-user".into(),
            role: UserRole::User,
        }
    }

    fn make_app(state: AppState, user: AuthenticatedUser) -> Router {
        Router::new()
            .route("/run", post(run_handler))
            .route("/run/{id}/stop", post(stop_handler))
            .layer(axum::middleware::from_fn(
                move |mut req: axum::extract::Request, next: axum::middleware::Next| {
                    let user = user.clone();
                    async move {
                        req.extensions_mut().insert(user);
                        next.run(req).await
                    }
                },
            ))
            .with_state(state)
    }

    // ── Negative tests: malformed inputs ─────────────────────────────────

    /// POST /run with missing bundle_id returns 400.
    #[tokio::test]
    async fn run_missing_bundle_id_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let _ = Registry::open(dir.path()).unwrap();
        let state = make_test_state(dir.path().to_path_buf());
        let app = make_app(state, make_test_user());

        let req = Request::builder()
            .method("POST")
            .uri("/run")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"bundle_id":"","mode":"sync","payload":{}}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "bad_request");
    }

    /// POST /run with invalid mode returns 400.
    #[tokio::test]
    async fn run_invalid_mode_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let _ = Registry::open(dir.path()).unwrap();
        let state = make_test_state(dir.path().to_path_buf());
        let app = make_app(state, make_test_user());

        let req = Request::builder()
            .method("POST")
            .uri("/run")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"bundle_id":"test","mode":"invalid","payload":{}}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("invalid mode"));
    }

    /// POST /run with empty body (no JSON) returns 400.
    #[tokio::test]
    async fn run_empty_body_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let _ = Registry::open(dir.path()).unwrap();
        let state = make_test_state(dir.path().to_path_buf());
        let app = make_app(state, make_test_user());

        let req = Request::builder()
            .method("POST")
            .uri("/run")
            .header("Content-Type", "application/json")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // Axum returns 400 for missing/invalid JSON body
        assert!(
            resp.status() == StatusCode::BAD_REQUEST
                || resp.status() == StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    // ── Negative tests: error paths ──────────────────────────────────────

    /// POST /run with unknown bundle returns 404.
    #[tokio::test]
    async fn run_unknown_bundle_returns_404() {
        let dir = tempfile::tempdir().unwrap();
        let _ = Registry::open(dir.path()).unwrap();
        let state = make_test_state(dir.path().to_path_buf());
        let app = make_app(state, make_test_user());

        let req = Request::builder()
            .method("POST")
            .uri("/run")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"bundle_id":"nonexistent","mode":"sync","payload":{"key":"value"}}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "not_found");
    }

    /// POST /run with valid bundle but no workers returns 503.
    #[tokio::test]
    async fn run_no_workers_returns_503() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();

        // Register a platform bundle so the bundle check passes
        let new_image = hyphae_core::registry::NewImage {
            content_hash: "abc123".into(),
            name: "test-app".into(),
            tag: "latest".into(),
            size_bytes: 1024,
            source_path: "/tmp/src".into(),
            init_config: None,
            disk_path: "/tmp/test.ext4".into(),
            created_at: 1000,
            default_vcpus: 2,
            default_memory_mib: 256,
            owner_id: None, // Platform bundle
            manifest_resources: None,
            manifest_env: None,
            manifest_secrets: None,
            manifest_capabilities: None,
            manifest_a2a: None,
            manifest_timeout_secs: None,
            manifest_volumes: None,
        };
        registry.register_image(&new_image).unwrap();
        drop(registry);

        let mut state = make_test_state(dir.path().to_path_buf());
        // Set worker_registry but with no workers registered
        state.worker_registry = Some(Arc::new(crate::worker::registry::WorkerRegistry::new()));

        let app = make_app(state, make_test_user());

        let req = Request::builder()
            .method("POST")
            .uri("/run")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"bundle_id":"test-app","mode":"sync","payload":{"key":"value"}}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "service_unavailable");
    }

    // ── Negative tests: stop handler ─────────────────────────────────────

    /// POST /run/{id}/stop with no workers returns 503.
    #[tokio::test]
    async fn stop_no_registry_returns_503() {
        let dir = tempfile::tempdir().unwrap();
        let _ = Registry::open(dir.path()).unwrap();
        let state = make_test_state(dir.path().to_path_buf());
        let app = make_app(state, make_test_user());

        let id = Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/run/{id}/stop"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// POST /run/{id}/stop with invalid UUID returns 400.
    #[tokio::test]
    async fn stop_invalid_uuid_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let _ = Registry::open(dir.path()).unwrap();
        let mut state = make_test_state(dir.path().to_path_buf());
        state.worker_registry = Some(Arc::new(crate::worker::registry::WorkerRegistry::new()));
        let app = make_app(state, make_test_user());

        let req = Request::builder()
            .method("POST")
            .uri("/run/not-a-uuid/stop")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// POST /run/{id}/stop with unknown invocation and empty registry returns 503.
    #[tokio::test]
    async fn stop_unknown_invocation_empty_workers_returns_503() {
        let dir = tempfile::tempdir().unwrap();
        let _ = Registry::open(dir.path()).unwrap();
        let mut state = make_test_state(dir.path().to_path_buf());
        state.worker_registry = Some(Arc::new(crate::worker::registry::WorkerRegistry::new()));
        let app = make_app(state, make_test_user());

        let id = Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/run/{id}/stop"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // No workers → 503
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
