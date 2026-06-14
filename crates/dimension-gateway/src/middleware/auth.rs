//! Bearer token authentication middleware.
//!
//! Validates the `Authorization: Bearer <token>` header against the database.
//! Two auth paths are supported:
//!
//! 1. **Legacy**: token matches `config.token` (DIMENSION_TOKEN env var) → resolves
//!    to the bootstrap admin for backward compatibility with existing deployments.
//! 2. **DB-backed**: token is SHA-256 hashed and looked up in the `api_keys` table.
//!    Returns `AuthenticatedUser` on success; 401 on any failure.
//!
//! The `AuthenticatedUser` is injected into request extensions for downstream handlers.

use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use sha2::Digest;

use crate::models::error::AppError;
use crate::server::AppState;

/// DB-backed bearer token auth middleware for use with `from_fn_with_state`.
///
/// # Auth flow
///
/// 1. Extract `Authorization: Bearer <token>` header. Missing or malformed → 401.
/// 2. If token matches `config.token` (legacy DIMENSION_TOKEN): resolve bootstrap admin → inject.
/// 3. Otherwise: SHA-256 hash the token, look up in `api_keys` table. 401 on any failure.
///
/// # Security
///
/// - DB lookup failures (KeyNotFound, revoked, deleted) all return 401 — no leakage.
/// - Bootstrap path preserves backward compat for operators who haven't migrated.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let auth_header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let token = match auth_header {
        Some(value) if value.starts_with("Bearer ") => &value[7..],
        _ => return Err(AppError::Unauthorized),
    };

    // Backward compatibility: config.token (DIMENSION_TOKEN) authenticates as bootstrap admin.
    // This MUST come before the DB lookup path.
    if token == state.config.token {
        let bootstrap_user = state
            .user_store
            .get_bootstrap_admin()
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        request.extensions_mut().insert(bootstrap_user);
        return Ok(next.run(request).await);
    }

    // DB-backed path: hash the token, look up in api_keys.
    // On any failure (key not found, revoked, user deleted, DB error) → 401.
    // Never leak internal error details.
    let key_hash = hex::encode(sha2::Sha256::digest(token.as_bytes()));
    let user = state
        .user_store
        .authenticate_key(&key_hash)
        .await
        .map_err(|_| AppError::Unauthorized)?;

    request.extensions_mut().insert(user);
    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request as HttpRequest, StatusCode},
        middleware,
        routing::get,
        Router,
    };
    use dimension_store::{
        ApiKey, AuthenticatedUser, StoreError, User, UserRole, UserStore,
    };
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::config::AppConfig;
    use crate::resilience::ConcurrencyController;
    use crate::server::AppState;

    // ── MockUserStore ──────────────────────────────────────────────────────

    /// A simple mock UserStore for unit tests.
    /// - authenticate_key: returns success for KNOWN_KEY_HASH, KeyNotFound otherwise.
    /// - get_bootstrap_admin: returns a hardcoded admin user.
    /// - All other methods: unimplemented (tests don't call them).
    struct MockUserStore;

    const KNOWN_KEY: &str = "test-db-key-12345";
    fn known_key_hash() -> String {
        hex::encode(sha2::Sha256::digest(KNOWN_KEY.as_bytes()))
    }

    fn mock_user(name: &str, role: UserRole) -> AuthenticatedUser {
        AuthenticatedUser {
            user_id: Uuid::new_v4(),
            name: name.to_string(),
            role,
        }
    }

    #[async_trait]
    impl UserStore for MockUserStore {
        async fn create_user(
            &self,
            _name: &str,
            _role: UserRole,
        ) -> Result<(User, String), StoreError> {
            unimplemented!()
        }

        async fn get_user(&self, _user_id: Uuid) -> Result<Option<User>, StoreError> {
            unimplemented!()
        }

        async fn list_users(&self) -> Result<Vec<User>, StoreError> {
            unimplemented!()
        }

        async fn soft_delete_user(&self, _user_id: Uuid) -> Result<(), StoreError> {
            unimplemented!()
        }

        async fn promote_user(&self, _user_id: Uuid) -> Result<(), StoreError> {
            unimplemented!()
        }

        async fn demote_user(&self, _user_id: Uuid) -> Result<(), StoreError> {
            unimplemented!()
        }

        async fn admin_count(&self) -> Result<i64, StoreError> {
            unimplemented!()
        }

        async fn create_key(
            &self,
            _user_id: Uuid,
            _label: Option<&str>,
        ) -> Result<(ApiKey, String), StoreError> {
            unimplemented!()
        }

        async fn authenticate_key(&self, key_hash: &str) -> Result<AuthenticatedUser, StoreError> {
            if key_hash == known_key_hash() {
                Ok(mock_user("db-user", UserRole::User))
            } else {
                Err(StoreError::KeyNotFound)
            }
        }

        async fn revoke_key(&self, _key_id: Uuid) -> Result<(), StoreError> {
            unimplemented!()
        }

        async fn list_keys_for_user(&self, _user_id: Uuid) -> Result<Vec<ApiKey>, StoreError> {
            Ok(vec![])
        }

        async fn ensure_bootstrap_admin(&self) -> Result<Option<String>, StoreError> {
            unimplemented!()
        }

        async fn get_bootstrap_admin(&self) -> Result<AuthenticatedUser, StoreError> {
            Ok(mock_user("bootstrap-admin", UserRole::Admin))
        }

    }

    // ── Test helpers ──────────────────────────────────────────────────────

    fn test_config() -> AppConfig {
        AppConfig {
            port: 3000,
            host: "127.0.0.1".into(),
            token: "test-secret-token".into(),
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
        }
    }

    fn test_state() -> AppState {
        let config = test_config();
        let temp_dir = tempfile::tempdir().unwrap();
        let registry_path = temp_dir.path().to_path_buf();
        std::mem::forget(temp_dir);
        AppState {
            concurrency_controller: Arc::new(ConcurrencyController::new(config.max_concurrent)),
            resource_caps: config.resource_caps(),
            user_store: Arc::new(MockUserStore),
            session_store: Arc::new(crate::test_utils::MockSessionStore::new()),
            bundle_job_store: crate::bundle_store::BundleJobStore::new(),
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

    fn test_app() -> Router {
        let state = test_state();
        Router::new()
            .route("/protected", get(|| async { "ok" }))
            .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
            .with_state(state)
    }

    // ── Existing tests (behavior unchanged) ───────────────────────────────

    #[tokio::test]
    async fn valid_bearer_token_passes() {
        let app = test_app();
        let req = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Bearer test-secret-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn missing_auth_header_returns_401() {
        let app = test_app();
        let req = HttpRequest::builder()
            .uri("/protected")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn wrong_token_returns_401() {
        let app = test_app();
        let req = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Bearer wrong-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn non_bearer_scheme_returns_401() {
        let app = test_app();
        let req = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Basic dXNlcjpwYXNz")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn empty_bearer_value_returns_401() {
        let app = test_app();
        let req = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Bearer ")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // ── New tests ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_db_backed_key_authenticates() {
        let app = test_app();
        let req = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", format!("Bearer {}", KNOWN_KEY))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "known DB-backed key should authenticate"
        );
    }

    #[tokio::test]
    async fn test_legacy_token_auth() {
        let app = test_app();
        // Use the config.token (legacy DIMENSION_TOKEN path)
        let req = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Bearer test-secret-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "legacy config.token should still authenticate"
        );
    }

    #[tokio::test]
    async fn test_unknown_key_returns_401() {
        let app = test_app();
        let req = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Bearer unknown-key-not-in-db")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "unknown key should return 401"
        );
    }

    #[tokio::test]
    async fn test_forbidden_error_returns_403() {
        use crate::models::error::AppError;
        use axum::response::IntoResponse;

        let err = AppError::Forbidden("".into());
        let resp = err.into_response();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(json["error"]["code"], "forbidden");
    }
}
