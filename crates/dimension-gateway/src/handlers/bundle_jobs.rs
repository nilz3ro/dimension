//! Bundle job status HTTP handler.
//!
//! - [`get_job_handler`]: GET /bundles/jobs/{id} - poll async bundle conversion job status

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use crate::models::error::AppError;
use crate::server::AppState;

/// Response body for GET /bundles/jobs/{id}.
#[derive(Debug, Serialize)]
pub struct JobStatusResponse {
    pub id: Uuid,
    /// Serialized from JobStatus (snake_case).
    pub status: String,
    /// Serialized from JobStage (snake_case).
    pub stage: String,
    pub progress_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// GET /bundles/jobs/{id} -- poll bundle conversion job status.
///
/// Returns 200 with the current job record, or 404 if the job ID is unknown.
pub async fn get_job_handler(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<JobStatusResponse>, AppError> {
    let record = state
        .bundle_job_store
        .get(&job_id)
        .ok_or_else(|| AppError::NotFound(format!("job not found: {job_id}")))?;

    // Serialize enum variants to lowercase strings via serde_json.
    // JobStatus and JobStage have #[serde(rename_all = "snake_case")] so
    // serde_json::to_value serializes them as lowercase strings already.
    let status = serde_json::to_value(&record.status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{:?}", record.status).to_lowercase());

    let stage = serde_json::to_value(&record.stage)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{:?}", record.stage).to_lowercase());

    Ok(Json(JobStatusResponse {
        id: record.id,
        status,
        stage,
        progress_message: record.progress_message,
        bundle_id: record.bundle_id,
        error: record.error,
        created_at: record.created_at,
        updated_at: record.updated_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use dimension_store::{AuthenticatedUser, UserRole};
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

    struct NoopUserStore;

    #[async_trait::async_trait]
    impl dimension_store::UserStore for NoopUserStore {
        async fn create_user(
            &self,
            _n: &str,
            _r: UserRole,
        ) -> Result<(dimension_store::User, String), dimension_store::StoreError> {
            unimplemented!()
        }
        async fn get_user(
            &self,
            _id: Uuid,
        ) -> Result<Option<dimension_store::User>, dimension_store::StoreError> {
            unimplemented!()
        }
        async fn list_users(
            &self,
        ) -> Result<Vec<dimension_store::User>, dimension_store::StoreError> {
            unimplemented!()
        }
        async fn soft_delete_user(&self, _: Uuid) -> Result<(), dimension_store::StoreError> {
            unimplemented!()
        }
        async fn promote_user(&self, _: Uuid) -> Result<(), dimension_store::StoreError> {
            unimplemented!()
        }
        async fn demote_user(&self, _: Uuid) -> Result<(), dimension_store::StoreError> {
            unimplemented!()
        }
        async fn admin_count(&self) -> Result<i64, dimension_store::StoreError> {
            unimplemented!()
        }
        async fn create_key(
            &self,
            _: Uuid,
            _: Option<&str>,
        ) -> Result<(dimension_store::ApiKey, String), dimension_store::StoreError> {
            unimplemented!()
        }
        async fn authenticate_key(
            &self,
            _: &str,
        ) -> Result<AuthenticatedUser, dimension_store::StoreError> {
            unimplemented!()
        }
        async fn revoke_key(&self, _: Uuid) -> Result<(), dimension_store::StoreError> {
            unimplemented!()
        }
        async fn list_keys_for_user(&self, _: Uuid) -> Result<Vec<dimension_store::ApiKey>, dimension_store::StoreError> {
            Ok(vec![])
        }
        async fn ensure_bootstrap_admin(
            &self,
        ) -> Result<Option<String>, dimension_store::StoreError> {
            unimplemented!()
        }
        async fn get_bootstrap_admin(
            &self,
        ) -> Result<AuthenticatedUser, dimension_store::StoreError> {
            unimplemented!()
        }
    }

    fn test_state_with_store(store: BundleJobStore) -> AppState {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry_path = temp_dir.path().to_path_buf();
        std::mem::forget(temp_dir);

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
            registry_path: None,
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
            concurrency_controller: Arc::new(ConcurrencyController::new(config.max_concurrent)),
            resource_caps: config.resource_caps(),
            user_store: Arc::new(NoopUserStore),
            session_store: Arc::new(crate::test_utils::MockSessionStore::new()),
            bundle_job_store: store,
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

    struct NoopVolumeStore;
    #[async_trait::async_trait]
    impl dimension_store::VolumeStore for NoopVolumeStore {
        async fn create_volume(&self, _u: uuid::Uuid, _s: i64) -> Result<dimension_store::Volume, dimension_store::StoreError> { unimplemented!() }
        async fn get_volume(&self, _id: uuid::Uuid) -> Result<Option<dimension_store::Volume>, dimension_store::StoreError> { Ok(None) }
        async fn list_volumes_for_user(&self, _u: uuid::Uuid) -> Result<Vec<dimension_store::Volume>, dimension_store::StoreError> { Ok(vec![]) }
        async fn attach_volume(&self, _vid: uuid::Uuid, _sid: uuid::Uuid) -> Result<dimension_store::Volume, dimension_store::StoreError> { unimplemented!() }
        async fn detach_volume(&self, _vid: uuid::Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn set_worker(&self, _vid: uuid::Uuid, _w: Option<&str>) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn delete_volume(&self, _vid: uuid::Uuid, _uid: uuid::Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn find_volume_for_session(&self, _sid: uuid::Uuid) -> Result<Option<dimension_store::Volume>, dimension_store::StoreError> { Ok(None) }
        async fn touch_volume(&self, _vid: uuid::Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
    }

    struct NoopArtifactStore;
    #[async_trait::async_trait]
    impl dimension_store::ArtifactStore for NoopArtifactStore {
        async fn put_artifact(&self, _op: &opendal::Operator, _sid: uuid::Uuid, _uid: uuid::Uuid, _key: &str, _data: bytes::Bytes, _ct: Option<&str>) -> Result<dimension_store::Artifact, dimension_store::StoreError> { unimplemented!() }
        async fn list_artifacts_for_session(&self, _sid: uuid::Uuid, _uid: uuid::Uuid) -> Result<Vec<dimension_store::Artifact>, dimension_store::StoreError> { Ok(vec![]) }
        async fn get_artifact(&self, _sid: uuid::Uuid, _uid: uuid::Uuid, _key: &str) -> Result<Option<dimension_store::Artifact>, dimension_store::StoreError> { Ok(None) }
        async fn delete_artifact(&self, _op: &opendal::Operator, _sid: uuid::Uuid, _uid: uuid::Uuid, _key: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn list_artifacts_for_user(&self, _uid: uuid::Uuid, _cursor: Option<&str>, _limit: i64) -> Result<(Vec<dimension_store::Artifact>, Option<String>), dimension_store::StoreError> { Ok((vec![], None)) }
        async fn admin_list_artifacts(&self, _cursor: Option<&str>, _limit: i64) -> Result<(Vec<dimension_store::Artifact>, Option<String>), dimension_store::StoreError> { Ok((vec![], None)) }
        async fn delete_expired_artifacts(&self, _op: &opendal::Operator) -> Result<u64, dimension_store::StoreError> { Ok(0) }
    }

    struct NoopDeploymentStore;
    #[async_trait::async_trait]
    impl dimension_store::DeploymentStore for NoopDeploymentStore {
        async fn create_deployment(&self, _n: dimension_store::NewDeployment) -> Result<dimension_store::Deployment, dimension_store::StoreError> { unimplemented!() }
        async fn get_deployment(&self, _id: uuid::Uuid, _u: uuid::Uuid) -> Result<Option<dimension_store::Deployment>, dimension_store::StoreError> { Ok(None) }
        async fn list_deployments(&self, _u: uuid::Uuid) -> Result<Vec<dimension_store::Deployment>, dimension_store::StoreError> { Ok(vec![]) }
        async fn update_deployment_status(&self, _id: uuid::Uuid, _s: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn update_deployment_worker(&self, _id: uuid::Uuid, _w: &str, _ip: &str, _pid: i32) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn increment_probe_failures(&self, _id: uuid::Uuid) -> Result<i32, dimension_store::StoreError> { Ok(0) }
        async fn reset_probe_failures(&self, _id: uuid::Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn list_active_on_worker(&self, _w: &str) -> Result<Vec<dimension_store::Deployment>, dimension_store::StoreError> { Ok(vec![]) }
        async fn mark_orphaned_for_worker(&self, _w: &str) -> Result<u64, dimension_store::StoreError> { Ok(0) }
        async fn get_deployment_public(&self, _id: uuid::Uuid) -> Result<Option<dimension_store::Deployment>, dimension_store::StoreError> { Ok(None) }
        async fn list_active_worker_ids(&self) -> Result<Vec<String>, dimension_store::StoreError> { Ok(vec![]) }
        async fn list_probeable_deployments(&self) -> Result<Vec<dimension_store::Deployment>, dimension_store::StoreError> { Ok(vec![]) }
    }

    struct NoopNamedVolumeStore;
    #[async_trait::async_trait]
    impl dimension_store::NamedVolumeStore for NoopNamedVolumeStore {
        async fn create_named_volume(&self, _u: uuid::Uuid, _n: &str, _s: i64) -> Result<dimension_store::NamedVolume, dimension_store::StoreError> { unimplemented!() }
        async fn get_named_volume(&self, _id: uuid::Uuid) -> Result<Option<dimension_store::NamedVolume>, dimension_store::StoreError> { Ok(None) }
        async fn find_named_volume_by_name(&self, _u: uuid::Uuid, _n: &str) -> Result<Option<dimension_store::NamedVolume>, dimension_store::StoreError> { Ok(None) }
        async fn list_named_volumes_for_user(&self, _u: uuid::Uuid) -> Result<Vec<dimension_store::NamedVolume>, dimension_store::StoreError> { Ok(vec![]) }
        async fn set_named_volume_worker(&self, _id: uuid::Uuid, _w: Option<&str>) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn delete_named_volume(&self, _id: uuid::Uuid, _u: uuid::Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
    }

    struct NoopStorageStore;
    #[async_trait::async_trait]
    impl dimension_store::StorageStore for NoopStorageStore {
        async fn get_storage_info(&self, _u: uuid::Uuid, _b: &str) -> Result<(i64, i64), dimension_store::StoreError> { Ok((0, 104857600)) }
        async fn increment_bytes_used(&self, _u: uuid::Uuid, _b: &str, _d: i64) -> Result<i64, dimension_store::StoreError> { Ok(0) }
        async fn decrement_bytes_used(&self, _u: uuid::Uuid, _b: &str, _d: i64) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn set_quota(&self, _u: uuid::Uuid, _b: &str, _q: i64) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn list_storage_stats(&self) -> Result<Vec<dimension_store::BundleStorageRecord>, dimension_store::StoreError> { Ok(vec![]) }
        async fn set_bytes_used(&self, _u: uuid::Uuid, _b: &str, _bytes: i64) -> Result<(), dimension_store::StoreError> { Ok(()) }
    }

    struct NoopTaskStore;
    #[async_trait::async_trait]
    impl dimension_store::TaskStore for NoopTaskStore {
        async fn upsert_agent_card(&self, _b: &str, _u: uuid::Uuid, _j: serde_json::Value) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn get_agent_card(&self, _b: &str) -> Result<Option<serde_json::Value>, dimension_store::StoreError> { Ok(None) }
        async fn delete_agent_card(&self, _b: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn get_or_create_agent_session(&self, _c: &str, _t: &str, _u: uuid::Uuid, _ss: &dyn dimension_store::SessionStore) -> Result<uuid::Uuid, dimension_store::StoreError> { Ok(uuid::Uuid::new_v4()) }
        async fn create_task(&self, _t: dimension_store::NewTask) -> Result<dimension_store::Task, dimension_store::StoreError> { unimplemented!() }
        async fn get_task(&self, _id: uuid::Uuid, _u: uuid::Uuid) -> Result<Option<dimension_store::Task>, dimension_store::StoreError> { Ok(None) }
        async fn get_task_scoped(&self, _id: uuid::Uuid, _u: uuid::Uuid, _b: &str) -> Result<Option<dimension_store::Task>, dimension_store::StoreError> { Ok(None) }
        async fn list_tasks(&self, _u: uuid::Uuid, _s: Option<&str>, _c: Option<&str>, _l: i64) -> Result<(Vec<dimension_store::Task>, Option<String>), dimension_store::StoreError> { Ok((vec![], None)) }
        async fn update_task_status(&self, _id: uuid::Uuid, _s: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn cancel_task(&self, _id: uuid::Uuid, _u: uuid::Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn claim_ready_tasks(&self, _l: i32) -> Result<Vec<dimension_store::Task>, dimension_store::StoreError> { Ok(vec![]) }
        async fn complete_task_iteration(&self, _id: uuid::Uuid, _r: dimension_store::NewTaskRun) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn get_task_runs(&self, _id: uuid::Uuid, _u: uuid::Uuid) -> Result<Vec<dimension_store::TaskRun>, dimension_store::StoreError> { Ok(vec![]) }
        async fn count_running_tasks(&self, _u: uuid::Uuid) -> Result<i64, dimension_store::StoreError> { Ok(0) }
        async fn get_task_targets(&self, _id: uuid::Uuid) -> Result<Vec<String>, dimension_store::StoreError> { Ok(vec![]) }
        async fn set_task_next_run(&self, _id: uuid::Uuid, _t: chrono::DateTime<chrono::Utc>) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn admin_list_tasks(&self, _s: Option<&str>, _c: Option<&str>, _l: i64) -> Result<(Vec<dimension_store::Task>, Option<String>), dimension_store::StoreError> { Ok((vec![], None)) }
        async fn admin_get_task(&self, _id: uuid::Uuid) -> Result<Option<dimension_store::Task>, dimension_store::StoreError> { Ok(None) }
        async fn retry_task(&self, _id: uuid::Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
    }

    struct NoopSecretStore;
    #[async_trait::async_trait]
    impl dimension_store::SecretStore for NoopSecretStore {
        async fn upsert_secret_metadata(&self, _u: uuid::Uuid, _b: &str, _n: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn list_secret_metadata(&self, _u: uuid::Uuid, _b: &str) -> Result<Vec<dimension_store::SecretMetadata>, dimension_store::StoreError> { Ok(vec![]) }
        async fn delete_secret_metadata(&self, _u: uuid::Uuid, _b: &str, _n: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn insert_token(&self, _id: &str, _bid: &str, _u: uuid::Uuid, _c: &str, _e: Option<chrono::DateTime<chrono::Utc>>) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn get_token(&self, _id: &str, _b: &str) -> Result<Option<dimension_store::TokenRecord>, dimension_store::StoreError> { Ok(None) }
        async fn delete_expired_tokens(&self) -> Result<u64, dimension_store::StoreError> { Ok(0) }
    }

    fn make_app(store: BundleJobStore) -> Router {
        let state = test_state_with_store(store);
        Router::new()
            .route("/bundles/jobs/{id}", get(get_job_handler))
            .with_state(state)
    }

    /// GET /bundles/jobs/{id} with a known job ID returns 200 with all fields.
    #[tokio::test]
    async fn get_job_returns_200_with_all_fields() {
        let store = BundleJobStore::new();
        let job_id = Uuid::new_v4();
        store.create(job_id);

        let app = make_app(store);
        let req = Request::builder()
            .method("GET")
            .uri(format!("/bundles/jobs/{job_id}"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["id"], job_id.to_string());
        assert!(json["status"].is_string(), "status must be a string");
        assert!(json["stage"].is_string(), "stage must be a string");
        assert!(json["progress_message"].is_string());
        assert!(json["created_at"].is_number());
        assert!(json["updated_at"].is_number());
    }

    /// GET /bundles/jobs/{id} for a queued job serializes status as "queued".
    #[tokio::test]
    async fn get_job_queued_status_serializes_as_snake_case() {
        let store = BundleJobStore::new();
        let job_id = Uuid::new_v4();
        store.create(job_id);

        let app = make_app(store);
        let req = Request::builder()
            .method("GET")
            .uri(format!("/bundles/jobs/{job_id}"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["status"], "queued");
        assert_eq!(json["stage"], "queued");
        // bundle_id and error should be absent for a queued job
        assert!(json["bundle_id"].is_null(), "bundle_id should be absent for queued job");
        assert!(json["error"].is_null(), "error should be absent for queued job");
    }

    /// GET /bundles/jobs/{id} for a complete job has bundle_id.
    #[tokio::test]
    async fn get_job_complete_has_bundle_id() {
        let store = BundleJobStore::new();
        let job_id = Uuid::new_v4();
        store.create(job_id);
        store.complete(job_id, 42);

        let app = make_app(store);
        let req = Request::builder()
            .method("GET")
            .uri(format!("/bundles/jobs/{job_id}"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["status"], "complete");
        assert_eq!(json["bundle_id"], 42);
    }

    /// GET /bundles/jobs/{id} for a failed job has error field.
    #[tokio::test]
    async fn get_job_failed_has_error_field() {
        use crate::bundle_store::types::JobStage;

        let store = BundleJobStore::new();
        let job_id = Uuid::new_v4();
        store.create(job_id);
        store.fail(job_id, JobStage::Extracting, "path traversal detected");

        let app = make_app(store);
        let req = Request::builder()
            .method("GET")
            .uri(format!("/bundles/jobs/{job_id}"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["status"], "failed");
        assert!(json["error"].as_str().unwrap().contains("path traversal"));
    }

    /// GET /bundles/jobs/{unknown_id} returns 404.
    #[tokio::test]
    async fn get_job_unknown_id_returns_404() {
        let store = BundleJobStore::new();
        let unknown_id = Uuid::new_v4();

        let app = make_app(store);
        let req = Request::builder()
            .method("GET")
            .uri(format!("/bundles/jobs/{unknown_id}"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "not_found");
    }
}
