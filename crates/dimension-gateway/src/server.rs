//! Router construction and shared application state.
//!
//! [`build_router`] assembles the axum [`Router`] with:
//! - Protected routes: POST /messages (bearer auth required, concurrency limited)
//! - Admin routes: /admin/* (bearer auth + admin role check required)
//! - Public routes: GET /health (no auth)
//! - Request ID generation, tracing, and propagation middleware

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{any, delete, get, post, put};
use axum::Router;
use tokio_util::sync::CancellationToken;
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

use crate::bundle_store::BundleJobStore;
use crate::config::AppConfig;
use crate::worker::registry::WorkerRegistry;
use crate::handlers::workers::register_worker_handler;
use crate::handlers::admin::{
    artifacts::{admin_delete_artifact_handler, admin_list_artifacts_handler},
    bundles::{admin_delete_bundle_handler, admin_list_bundles_handler, admin_rebuild_bundle_handler},
    health::admin_health_handler,
    keys::{create_key_handler, list_keys_handler, revoke_key_handler},
    sessions::{
        admin_delete_session_handler, admin_get_session_handler, admin_list_sessions_handler,
        admin_search_sessions_handler,
    },
    storage::{
        admin_delete_object_handler, admin_get_object_handler, admin_list_all_storage_handler,
        admin_list_bundle_storage_handler, admin_set_quota_handler,
    },
    tasks::{admin_list_tasks_handler, admin_retry_task_handler},
    users::{
        create_user_handler, delete_user_handler, demote_user_handler, list_users_handler,
        promote_user_handler,
    },
    workers::{admin_drain_worker_handler, admin_list_workers_handler, admin_remove_worker_handler},
};
use crate::handlers::{
    create_secret_handler, delete_secret_handler, detokenize_handler, get_bundle_handler,
    get_job_handler, health_handler, list_bundles_handler, list_secrets_handler, logs_tail_handler,
    push_handler, rollback_handler, tokenize_handler, upload_handler,
    delete_storage_handler, get_storage_handler, list_storage_handler, put_storage_handler,
};
use crate::handlers::artifacts::{
    get_artifact_handler, list_artifacts_handler, list_artifacts_html_handler,
    list_user_artifacts_handler, user_artifacts_html_handler,
};
use crate::handlers::volumes::{
    create_volume_handler, delete_volume_handler, list_volumes_handler,
    create_named_volume_handler, list_named_volumes_handler, delete_named_volume_handler,
};
use crate::handlers::deployments::{
    create_deployment_handler, deployment_proxy_handler,
    list_deployments_handler, stop_deployment_handler,
};
use crate::handlers::invocations::get_invocation_handler;
use crate::handlers::run::{run_handler, stop_handler};
use crate::handlers::run_events::run_events_handler;
use crate::middleware::{admin_middleware, auth_middleware, concurrency_limit_middleware};
use crate::models::request::ResourceCaps;
use crate::observability::trace_layer;
use crate::resilience::ConcurrencyController;

/// Shared application state available to all handlers.
#[derive(Clone)]
pub struct AppState {
    /// Server configuration (token, host, port).
    pub config: AppConfig,
    /// Hot-reloadable concurrency controller for limiting concurrent VMs.
    pub concurrency_controller: Arc<ConcurrencyController>,
    /// Operator-configured resource caps for per-request VM sizing.
    pub resource_caps: ResourceCaps,
    /// When the gateway started accepting traffic (for uptime calculation).
    pub startup_time: Instant,
    /// Cancellation token for draining in-flight SSE streams during shutdown.
    /// Child of the shutdown token. When cancelled, active SSE streams terminate.
    pub drain_token: CancellationToken,
    /// User identity and API key store for DB-backed authentication.
    pub user_store: Arc<dyn dimension_store::UserStore>,
    /// Session persistence store.
    pub session_store: Arc<dyn dimension_store::SessionStore>,
    /// In-memory job store for tracking async bundle conversion jobs.
    pub bundle_job_store: BundleJobStore,
    /// Path to the hyphae registry data directory.
    /// Used by bundle handlers to open the Registry via spawn_blocking.
    /// (Registry uses rusqlite which is not Send+Sync, so it cannot be stored directly.)
    pub registry_path: PathBuf,
    /// Optional Vault client for secrets, tokenization, and VM token minting.
    /// None when Vault is not configured or failed to connect (degraded mode).
    pub vault_client: Option<Arc<crate::vault::VaultClient>>,

    /// Secret metadata and tokenization record store.
    pub secret_store: Arc<dyn dimension_store::SecretStore>,

    /// Optional MinIO storage client for per-bundle object storage.
    /// None when MinIO is not configured or failed to connect (degraded mode).
    pub storage_client: Option<Arc<crate::storage::MinioClient>>,

    /// Bundle object storage quota tracking store.
    pub storage_store: Arc<dyn dimension_store::StorageStore>,

    /// A2A agent card + inter-agent session + task orchestration store.
    pub task_store: Arc<dyn dimension_store::TaskStore>,

    /// Optional worker registry for multi-host worker tracking.
    /// Some when DIMENSION_MULTI_HOST=true, None in single-host mode.
    pub worker_registry: Option<Arc<WorkerRegistry>>,

    /// Persistent volume store for volume lifecycle management (Phase 17).
    pub volume_store: Arc<dyn dimension_store::VolumeStore>,

    /// Artifact metadata store for browsing and download endpoints (Phase 19).
    pub artifact_store: Arc<dyn dimension_store::ArtifactStore>,

    /// Deployment lifecycle store for long-running deployment VMs (Phase 24).
    pub deployment_store: Arc<dyn dimension_store::DeploymentStore>,

    /// Named shared volume store for persistent cross-session volumes (Phase 26).
    pub named_volume_store: Arc<dyn dimension_store::NamedVolumeStore>,

    /// In-memory log broadcaster for live log streaming via GET /admin/logs/tail.
    /// Every tracing event is forwarded here by BroadcastLayer in init_tracing().
    pub log_broadcaster: crate::observability::LogBroadcaster,

    /// Shared HTTP client for webhook dispatch.
    pub http_client: reqwest::Client,

    /// Optional Clickhouse client for invocation telemetry queries.
    /// None when Clickhouse is not configured or failed to connect (degraded mode).
    pub clickhouse_client: Option<clickhouse::Client>,

    /// Optional opendal operator for invocation log retrieval from MinIO.
    /// None when log MinIO is not configured (degraded mode).
    pub log_storage_client: Option<opendal::Operator>,

    /// Optional Pulsar consumer client for live run-event tailing.
    /// When None, the run-events SSE endpoint falls back to the worker's
    /// SubscribeRunEvents gRPC for live updates.
    pub pulsar_client: Option<crate::pulsar::PulsarClient>,
}

/// Build the axum router with all routes and middleware.
///
/// Route layout:
/// - `POST /messages` -- concurrency limited, then bearer auth, then handler
/// - `GET|POST|DELETE /admin/*` -- bearer auth + admin role check, then handler
/// - `GET /health` -- public, no authentication required
///
/// Middleware stack on `/messages` and `/admin/*` (axum layers execute bottom-to-top):
/// 1. (innermost for admin) `admin_middleware` via `route_layer` -- checks Admin role
/// 2. (innermost for messages) `auth_middleware` -- validates bearer token
/// 3. (outermost) `concurrency_limit_middleware` -- acquires permit or 503
///
/// Admin sub-router uses `route_layer(admin_middleware)` (NOT `layer`) so that
/// requests to non-existent `/admin/*` paths return 404 (not 403), avoiding
/// leaking that the `/admin` prefix exists.
///
/// Execution order for `/admin/users`: concurrency -> auth (inserts AuthenticatedUser)
/// -> admin (reads AuthenticatedUser, checks role) -> handler.
///
/// Global middleware (applied via `ServiceBuilder`, outermost-first):
/// 1. `SetRequestIdLayer` -- generates UUID v4, sets `x-request-id` header
/// 2. `trace_layer()` -- custom `TraceLayer` with `MakeSpan` reading `x-request-id`
/// 3. `PropagateRequestIdLayer` -- copies `x-request-id` to response headers
pub fn build_router(state: AppState) -> Router {
    // Get the semaphore for the concurrency middleware.
    let semaphore = state.concurrency_controller.semaphore();

    // Admin sub-router: all /admin/* routes with admin role guard.
    // route_layer (not layer!) ensures 404 for non-existent admin paths,
    // rather than 403 which would leak the /admin prefix exists.
    let admin = Router::new()
        .route("/users", post(create_user_handler).get(list_users_handler))
        .route("/users/{id}", delete(delete_user_handler))
        .route("/users/{id}/keys", post(create_key_handler).get(list_keys_handler))
        .route("/users/{id}/keys/{kid}/revoke", post(revoke_key_handler))
        .route("/users/{id}/promote", post(promote_user_handler))
        .route("/users/{id}/demote", post(demote_user_handler))
        // Storage admin endpoints (bypass capability gate, admin has full access).
        .route("/storage", get(admin_list_all_storage_handler))
        .route(
            "/storage/{user_id}/{bundle_id}",
            get(admin_list_bundle_storage_handler),
        )
        .route(
            "/storage/{user_id}/{bundle_id}/{*key}",
            get(admin_get_object_handler).delete(admin_delete_object_handler),
        )
        .route(
            "/storage/{user_id}/{bundle_id}/quota",
            put(admin_set_quota_handler),
        )
        // Session admin endpoints.
        // IMPORTANT: /sessions/search BEFORE /sessions/{id} to prevent "search" matching as id.
        .route("/sessions", get(admin_list_sessions_handler))
        .route("/sessions/search", get(admin_search_sessions_handler))
        .route(
            "/sessions/{id}",
            get(admin_get_session_handler).delete(admin_delete_session_handler),
        )
        // Bundle admin endpoints.
        .route("/bundles", get(admin_list_bundles_handler))
        .route("/bundles/{id}", delete(admin_delete_bundle_handler))
        .route("/bundles/{id}/rebuild", post(admin_rebuild_bundle_handler))
        // Worker admin endpoints.
        .route("/workers", get(admin_list_workers_handler))
        .route("/workers/{id}/drain", post(admin_drain_worker_handler))
        .route("/workers/{id}", delete(admin_remove_worker_handler))
        // Artifact admin endpoints.
        // NOTE: /sessions/{id}/artifacts/{*key} uses catch-all {*key} to support slash-containing keys.
        // Full path: /admin/sessions/{id}/artifacts/{*key} — does NOT conflict with user-scoped route.
        .route("/artifacts", get(admin_list_artifacts_handler))
        .route("/sessions/{id}/artifacts/{*key}", delete(admin_delete_artifact_handler))
        // Task admin endpoints.
        .route("/tasks", get(admin_list_tasks_handler))
        .route("/tasks/{id}/retry", post(admin_retry_task_handler))
        // Admin health check (comprehensive platform view).
        .route("/health", get(admin_health_handler))
        // Live log streaming — SSE endpoint, admin auth required.
        .route("/logs/tail", get(logs_tail_handler))
        .route_layer(middleware::from_fn(admin_middleware));

    // Bundle upload/push needs a larger body limit (2 GiB for Docker image tars / ext4 rootfs).
    // The global default is 2MB, which would reject large Docker image tars before
    // they reach the handler. DefaultBodyLimit::max(2 GiB) overrides this for upload only.
    let upload_routes = Router::new()
        .route("/bundles/upload", post(upload_handler))
        .route("/bundles/push", post(push_handler))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024 * 1024));

    // Protected routes: concurrency limit (outermost) -> auth -> handler.
    // Layers execute bottom-to-top: auth first in code = innermost = runs second.
    // Concurrency last in code = outermost = runs first.
    let protected = Router::new()
        // Bundle deployment endpoints (upload, listing, job polling).
        .route("/bundles/jobs/{id}", get(get_job_handler))
        .route("/bundles", get(list_bundles_handler))
        .route("/bundles/{id}", get(get_bundle_handler))
        .route("/bundles/{name}/rollback", post(rollback_handler))
        // Secret CRUD endpoints (auth required, scoped by user_id).
        .route(
            "/bundles/{id}/secrets",
            post(create_secret_handler).get(list_secrets_handler),
        )
        .route(
            "/bundles/{id}/secrets/{name}",
            delete(delete_secret_handler),
        )
        // Tokenize/detokenize endpoints (gated by capabilities.tokenize).
        .route("/bundles/{id}/tokenize", post(tokenize_handler))
        .route("/bundles/{id}/detokenize", post(detokenize_handler))
        // Storage CRUD endpoints (auth required, gated by capabilities.storage).
        .route("/bundles/{id}/storage", get(list_storage_handler))
        .route(
            "/bundles/{id}/storage/{*key}",
            get(get_storage_handler)
                .put(put_storage_handler)
                .delete(delete_storage_handler),
        )
        // Volume management endpoints (auth required, Phase 17).
        .route("/volumes", post(create_volume_handler).get(list_volumes_handler))
        .route("/volumes/{id}", delete(delete_volume_handler))
        // Named shared volume endpoints (auth required, Phase 26).
        .route("/named-volumes", post(create_named_volume_handler).get(list_named_volumes_handler))
        .route("/named-volumes/{id}", delete(delete_named_volume_handler))
        // Artifact browsing and download endpoints (auth required, Phase 19).
        .route("/sessions/{id}/artifacts", get(list_artifacts_handler))
        .route("/sessions/{id}/artifacts/{*key}", get(get_artifact_handler))
        .route("/artifacts", get(list_user_artifacts_handler))
        // Artifact HTML browser (Phase 26 — trailing slash = HTML, no slash = JSON API).
        .route("/sessions/{id}/artifacts/", get(list_artifacts_html_handler))
        .route("/users/{id}/artifacts", get(user_artifacts_html_handler))
        // Deployment CRUD endpoints (auth required, Phase 24).
        .route("/deployments", post(create_deployment_handler).get(list_deployments_handler))
        .route("/deployments/{id}", delete(stop_deployment_handler))
        // Run invocation endpoints (auth required, Phase 27).
        .route("/run", post(run_handler))
        .route("/run/{id}/stop", post(stop_handler))
        // Server-Sent Events stream of run events (auth required).
        .route("/runs/{run_id}/events", get(run_events_handler))
        // Invocation observability endpoint (auth required, Phase 27).
        .route("/invocations/{id}", get(get_invocation_handler))
        .merge(upload_routes)
        .nest("/admin", admin)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            semaphore,
            concurrency_limit_middleware,
        ));

    // Public routes: no auth required.
    // Includes health check and A2A agent card (publicly discoverable per A2A spec).
    let public = Router::new()
        .route("/health", get(health_handler));

    // Internal routes: no auth (internal network only).
    // Workers register here; these are NOT exposed to the public internet.
    let internal = Router::new()
        .route("/internal/workers/register", post(register_worker_handler));

    // Deployment reverse proxy — no auth, catches all HTTP methods.
    // IMPORTANT: Must come AFTER all other routes to avoid shadowing /deployments/* CRUD.
    // Proxy traffic arrives without user credentials (deployment URL is public).
    let proxy = Router::new()
        .route("/{deployment_id}/{*path}", any(deployment_proxy_handler));

    // Merge protected and public route groups, then apply
    // request-id generation, tracing, and propagation middleware.
    Router::new()
        .merge(protected)
        .merge(public)
        .merge(internal)
        .merge(proxy)
        .layer(
            ServiceBuilder::new()
                // 1. Generate request ID (first -- so TraceLayer can read it).
                .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
                // 2. Trace with custom span that includes request_id.
                .layer(trace_layer())
                // 3. Copy request ID to response headers.
                .layer(PropagateRequestIdLayer::x_request_id()),
        )
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle_store::BundleJobStore;
    use crate::resilience::ConcurrencyController;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::Utc;
    use dimension_store::{
        ApiKey, AuthenticatedUser, SecretMetadata, StoreError, TokenRecord, User,
        UserRole, UserStore,
    };
    use http_body_util::BodyExt;
    use sha2::Digest;
    use std::time::Instant;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;

    // ── MockUserStore ─────────────────────────────────────────────────────────
    //
    // Realistic mock for admin endpoint testing:
    // - authenticate_key: admin for ADMIN_KEY, user for USER_KEY, 401 otherwise
    // - get_bootstrap_admin: returns admin (for legacy config.token auth path)
    // - create_user: returns a new user + api key
    // - list_users: returns a single user
    // - soft_delete_user / promote_user: succeed by default
    // - demote_user: returns LastAdminDemotion (to test the guard)
    // - create_key: returns a new key + api key
    // - revoke_key: succeeds by default

    const ADMIN_KEY: &str = "test-admin-key";
    const USER_KEY: &str = "test-user-key";

    fn hash(key: &str) -> String {
        hex::encode(sha2::Sha256::digest(key.as_bytes()))
    }

    struct MockUserStore;

    #[async_trait::async_trait]
    impl UserStore for MockUserStore {
        async fn create_user(
            &self,
            name: &str,
            role: UserRole,
        ) -> Result<(User, String), StoreError> {
            Ok((
                User {
                    id: Uuid::new_v4(),
                    name: name.to_string(),
                    role,
                    created_at: Utc::now(),
                    deleted_at: None,
                    quota_max_sessions: None,
                    quota_max_bundles: None,
                    quota_max_concurrent_vms: None,
                },
                "dim_sk_test_key_placeholder_abcdef1234567890abcdef1234567890ab".to_string(),
            ))
        }

        async fn get_user(&self, _user_id: Uuid) -> Result<Option<User>, StoreError> {
            Ok(Some(User {
                id: Uuid::nil(),
                name: "test-user".into(),
                role: UserRole::User,
                created_at: Utc::now(),
                deleted_at: None,
                quota_max_sessions: None,
                quota_max_bundles: None,
                quota_max_concurrent_vms: None,
            }))
        }

        async fn list_users(&self) -> Result<Vec<User>, StoreError> {
            Ok(vec![User {
                id: Uuid::new_v4(),
                name: "test-user".to_string(),
                role: UserRole::User,
                created_at: Utc::now(),
                deleted_at: None,
                quota_max_sessions: None,
                quota_max_bundles: None,
                quota_max_concurrent_vms: None,
            }])
        }

        async fn soft_delete_user(&self, _user_id: Uuid) -> Result<(), StoreError> {
            Ok(())
        }

        async fn promote_user(&self, _user_id: Uuid) -> Result<(), StoreError> {
            Ok(())
        }

        async fn demote_user(&self, _user_id: Uuid) -> Result<(), StoreError> {
            // Simulate the last-admin guard being triggered.
            Err(StoreError::LastAdminDemotion)
        }

        async fn admin_count(&self) -> Result<i64, StoreError> {
            Ok(1)
        }

        async fn create_key(
            &self,
            _user_id: Uuid,
            label: Option<&str>,
        ) -> Result<(ApiKey, String), StoreError> {
            Ok((
                ApiKey {
                    id: Uuid::new_v4(),
                    user_id: Uuid::nil(),
                    key_hash: "hashed".into(),
                    key_prefix: "dim_sk_test".into(),
                    label: label.map(str::to_string),
                    created_at: Utc::now(),
                    revoked_at: None,
                },
                "dim_sk_test_key_placeholder_abcdef1234567890abcdef1234567890ab".to_string(),
            ))
        }

        async fn authenticate_key(&self, key_hash: &str) -> Result<AuthenticatedUser, StoreError> {
            if key_hash == hash(ADMIN_KEY) {
                Ok(AuthenticatedUser {
                    user_id: Uuid::nil(),
                    name: "admin".into(),
                    role: UserRole::Admin,
                })
            } else if key_hash == hash(USER_KEY) {
                Ok(AuthenticatedUser {
                    user_id: Uuid::nil(),
                    name: "user".into(),
                    role: UserRole::User,
                })
            } else {
                Err(StoreError::KeyNotFound)
            }
        }

        async fn revoke_key(&self, _key_id: Uuid) -> Result<(), StoreError> {
            Ok(())
        }

        async fn list_keys_for_user(&self, _user_id: Uuid) -> Result<Vec<ApiKey>, StoreError> {
            Ok(vec![])
        }

        async fn ensure_bootstrap_admin(&self) -> Result<Option<String>, StoreError> {
            unimplemented!()
        }

        async fn get_bootstrap_admin(&self) -> Result<AuthenticatedUser, StoreError> {
            Ok(AuthenticatedUser {
                user_id: Uuid::nil(),
                name: "bootstrap".into(),
                role: UserRole::Admin,
            })
        }

    }

    // ── MockSecretStore ───────────────────────────────────────────────────────

    struct MockSecretStore;

    #[async_trait::async_trait]
    impl dimension_store::SecretStore for MockSecretStore {
        async fn upsert_secret_metadata(
            &self,
            _user_id: Uuid,
            _bundle_id: &str,
            _name: &str,
        ) -> Result<(), StoreError> {
            Ok(())
        }

        async fn list_secret_metadata(
            &self,
            _user_id: Uuid,
            _bundle_id: &str,
        ) -> Result<Vec<SecretMetadata>, StoreError> {
            Ok(vec![])
        }

        async fn delete_secret_metadata(
            &self,
            _user_id: Uuid,
            _bundle_id: &str,
            _name: &str,
        ) -> Result<(), StoreError> {
            Ok(())
        }

        async fn insert_token(
            &self,
            _id: &str,
            _bundle_id: &str,
            _user_id: Uuid,
            _ciphertext: &str,
            _expires_at: Option<chrono::DateTime<Utc>>,
        ) -> Result<(), StoreError> {
            Ok(())
        }

        async fn get_token(
            &self,
            _id: &str,
            _bundle_id: &str,
        ) -> Result<Option<TokenRecord>, StoreError> {
            Ok(None)
        }

        async fn delete_expired_tokens(&self) -> Result<u64, StoreError> {
            Ok(0)
        }
    }

    // ── MockStorageStore ──────────────────────────────────────────────────────

    struct MockStorageStore;

    #[async_trait::async_trait]
    impl dimension_store::StorageStore for MockStorageStore {
        async fn get_storage_info(
            &self,
            _user_id: uuid::Uuid,
            _bundle_id: &str,
        ) -> Result<(i64, i64), dimension_store::StoreError> {
            Ok((0, 104857600))
        }

        async fn increment_bytes_used(
            &self,
            _user_id: uuid::Uuid,
            _bundle_id: &str,
            _delta: i64,
        ) -> Result<i64, dimension_store::StoreError> {
            Ok(0)
        }

        async fn decrement_bytes_used(
            &self,
            _user_id: uuid::Uuid,
            _bundle_id: &str,
            _delta: i64,
        ) -> Result<(), dimension_store::StoreError> {
            Ok(())
        }

        async fn set_quota(
            &self,
            _user_id: uuid::Uuid,
            _bundle_id: &str,
            _quota_bytes: i64,
        ) -> Result<(), dimension_store::StoreError> {
            Ok(())
        }

        async fn list_storage_stats(
            &self,
        ) -> Result<Vec<dimension_store::BundleStorageRecord>, dimension_store::StoreError> {
            Ok(vec![])
        }

        async fn set_bytes_used(
            &self,
            _user_id: uuid::Uuid,
            _bundle_id: &str,
            _bytes: i64,
        ) -> Result<(), dimension_store::StoreError> {
            Ok(())
        }
    }

    // ── MockTaskStore ─────────────────────────────────────────────────────────

    struct MockTaskStore;

    #[async_trait::async_trait]
    impl dimension_store::TaskStore for MockTaskStore {
        async fn upsert_agent_card(&self, _b: &str, _u: uuid::Uuid, _j: serde_json::Value) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn get_agent_card(&self, _b: &str) -> Result<Option<serde_json::Value>, dimension_store::StoreError> { Ok(None) }
        async fn delete_agent_card(&self, _b: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn get_or_create_agent_session(&self, _caller: &str, _target: &str, _u: uuid::Uuid, _ss: &dyn dimension_store::SessionStore) -> Result<uuid::Uuid, dimension_store::StoreError> { Ok(uuid::Uuid::new_v4()) }
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

    // ── MockVolumeStore ───────────────────────────────────────────────────────

    struct MockVolumeStore;

    #[async_trait::async_trait]
    impl dimension_store::VolumeStore for MockVolumeStore {
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

    // ── MockArtifactStore ─────────────────────────────────────────────────────

    struct MockArtifactStore;

    #[async_trait::async_trait]
    impl dimension_store::ArtifactStore for MockArtifactStore {
        async fn put_artifact(&self, _op: &opendal::Operator, _sid: uuid::Uuid, _uid: uuid::Uuid, _key: &str, _data: bytes::Bytes, _ct: Option<&str>) -> Result<dimension_store::Artifact, dimension_store::StoreError> { unimplemented!() }
        async fn list_artifacts_for_session(&self, _sid: uuid::Uuid, _uid: uuid::Uuid) -> Result<Vec<dimension_store::Artifact>, dimension_store::StoreError> { Ok(vec![]) }
        async fn get_artifact(&self, _sid: uuid::Uuid, _uid: uuid::Uuid, _key: &str) -> Result<Option<dimension_store::Artifact>, dimension_store::StoreError> { Ok(None) }
        async fn delete_artifact(&self, _op: &opendal::Operator, _sid: uuid::Uuid, _uid: uuid::Uuid, _key: &str) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn list_artifacts_for_user(&self, _uid: uuid::Uuid, _cursor: Option<&str>, _limit: i64) -> Result<(Vec<dimension_store::Artifact>, Option<String>), dimension_store::StoreError> { Ok((vec![], None)) }
        async fn admin_list_artifacts(&self, _cursor: Option<&str>, _limit: i64) -> Result<(Vec<dimension_store::Artifact>, Option<String>), dimension_store::StoreError> { Ok((vec![], None)) }
        async fn delete_expired_artifacts(&self, _op: &opendal::Operator) -> Result<u64, dimension_store::StoreError> { Ok(0) }
    }

    struct MockNamedVolumeStore;
    #[async_trait::async_trait]
    impl dimension_store::NamedVolumeStore for MockNamedVolumeStore {
        async fn create_named_volume(&self, _u: uuid::Uuid, _n: &str, _s: i64) -> Result<dimension_store::NamedVolume, dimension_store::StoreError> { unimplemented!() }
        async fn get_named_volume(&self, _id: uuid::Uuid) -> Result<Option<dimension_store::NamedVolume>, dimension_store::StoreError> { Ok(None) }
        async fn find_named_volume_by_name(&self, _u: uuid::Uuid, _n: &str) -> Result<Option<dimension_store::NamedVolume>, dimension_store::StoreError> { Ok(None) }
        async fn list_named_volumes_for_user(&self, _u: uuid::Uuid) -> Result<Vec<dimension_store::NamedVolume>, dimension_store::StoreError> { Ok(vec![]) }
        async fn set_named_volume_worker(&self, _id: uuid::Uuid, _w: Option<&str>) -> Result<(), dimension_store::StoreError> { Ok(()) }
        async fn delete_named_volume(&self, _id: uuid::Uuid, _u: uuid::Uuid) -> Result<(), dimension_store::StoreError> { Ok(()) }
    }

    struct MockDeploymentStore;
    #[async_trait::async_trait]
    impl dimension_store::DeploymentStore for MockDeploymentStore {
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

    // ── Test state ────────────────────────────────────────────────────────────

    fn test_state() -> AppState {
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
        // Create a temporary registry dir for tests (Registry::open creates the DB).
        let temp_dir = tempfile::tempdir().unwrap();
        let registry_path = temp_dir.path().to_path_buf();
        // NOTE: TempDir is intentionally leaked here so the path stays valid for the test duration.
        std::mem::forget(temp_dir);
        AppState {
            concurrency_controller: Arc::new(ConcurrencyController::new(config.max_concurrent)),
            resource_caps: config.resource_caps(),
            user_store: Arc::new(MockUserStore),
            session_store: Arc::new(crate::test_utils::MockSessionStore::new()),
            bundle_job_store: BundleJobStore::new(),
            registry_path,
            config,
            startup_time: Instant::now(),
            drain_token: CancellationToken::new(),
            vault_client: None,
            secret_store: Arc::new(MockSecretStore),
            storage_client: None,
            storage_store: Arc::new(MockStorageStore),
            task_store: Arc::new(MockTaskStore),
            worker_registry: None,
            volume_store: Arc::new(MockVolumeStore),
            artifact_store: Arc::new(MockArtifactStore),
            deployment_store: Arc::new(MockDeploymentStore),
            named_volume_store: Arc::new(MockNamedVolumeStore),
            log_broadcaster: crate::observability::LogBroadcaster::new(),
            http_client: reqwest::Client::new(),
            clickhouse_client: None,
            log_storage_client: None,
            pulsar_client: None,
        }
    }

    fn admin_bearer() -> String {
        format!("Bearer {ADMIN_KEY}")
    }

    fn user_bearer() -> String {
        format!("Bearer {USER_KEY}")
    }

    // ── Existing tests (behavior unchanged) ───────────────────────────────────

    #[tokio::test]
    async fn health_endpoint_is_public() {
        let app = build_router(test_state());
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], "ok");
        // vault_client is None in test_state, so vault_status should be "not_configured".
        assert_eq!(json["vault_status"], "not_configured");
    }

    // ── Admin endpoint tests ──────────────────────────────────────────────────

    /// POST /admin/users with admin bearer → 201 with user_id, name, role, api_key.
    #[tokio::test]
    async fn test_admin_create_user() {
        let app = build_router(test_state());
        let req = Request::builder()
            .method("POST")
            .uri("/admin/users")
            .header("Content-Type", "application/json")
            .header("Authorization", admin_bearer())
            .body(Body::from(r#"{"name":"alice"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["user_id"].is_string(), "user_id must be present");
        assert_eq!(json["name"], "alice");
        assert_eq!(json["role"], "user");
        assert!(json["api_key"].is_string(), "api_key must be present");
    }

    /// GET /admin/users with admin bearer → 200 with users array.
    #[tokio::test]
    async fn test_admin_list_users() {
        let app = build_router(test_state());
        let req = Request::builder()
            .method("GET")
            .uri("/admin/users")
            .header("Authorization", admin_bearer())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["users"].is_array(), "users must be an array");
    }

    /// DELETE /admin/users/{id} with admin bearer → 200.
    #[tokio::test]
    async fn test_admin_delete_user() {
        let app = build_router(test_state());
        let user_id = Uuid::new_v4();
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/admin/users/{user_id}"))
            .header("Authorization", admin_bearer())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["message"], "user deleted");
    }

    /// POST /admin/users with non-admin bearer → 403 Forbidden.
    #[tokio::test]
    async fn test_non_admin_gets_403() {
        let app = build_router(test_state());
        let req = Request::builder()
            .method("POST")
            .uri("/admin/users")
            .header("Content-Type", "application/json")
            .header("Authorization", user_bearer())
            .body(Body::from(r#"{"name":"alice"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "forbidden");
    }

    /// POST /admin/users with no Authorization header → 401 Unauthorized.
    #[tokio::test]
    async fn test_unauthenticated_gets_401() {
        let app = build_router(test_state());
        let req = Request::builder()
            .method("POST")
            .uri("/admin/users")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"name":"alice"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// GET /admin/nonexistent with admin bearer → 404, NOT 403.
    ///
    /// This is the critical test proving `route_layer` is used (not `layer`).
    /// If `layer` were used instead, admin_middleware would intercept this
    /// unmatched request and return 403 before axum can 404 it.
    #[tokio::test]
    async fn test_admin_nonexistent_route_gets_404() {
        let app = build_router(test_state());
        let req = Request::builder()
            .method("GET")
            .uri("/admin/nonexistent")
            .header("Authorization", admin_bearer())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "admin_middleware must use route_layer — non-existent admin routes must 404, not 403"
        );
    }

    /// POST /admin/users/{id}/keys with admin bearer → 201 with key fields.
    #[tokio::test]
    async fn test_admin_create_key() {
        let app = build_router(test_state());
        let user_id = Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/admin/users/{user_id}/keys"))
            .header("Content-Type", "application/json")
            .header("Authorization", admin_bearer())
            .body(Body::from(r#"{"label":"CI"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["key_id"].is_string(), "key_id must be present");
        assert!(json["key_prefix"].is_string(), "key_prefix must be present");
        assert_eq!(json["label"], "CI");
        assert!(json["api_key"].is_string(), "api_key must be present");
    }

    /// POST /admin/users/{id}/keys/{kid}/revoke with admin bearer → 200.
    #[tokio::test]
    async fn test_admin_revoke_key() {
        let app = build_router(test_state());
        let user_id = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/admin/users/{user_id}/keys/{key_id}/revoke"))
            .header("Authorization", admin_bearer())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["message"], "key revoked");
    }

    /// POST /admin/users/{id}/promote with admin bearer → 200.
    #[tokio::test]
    async fn test_admin_promote_user() {
        let app = build_router(test_state());
        let user_id = Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/admin/users/{user_id}/promote"))
            .header("Authorization", admin_bearer())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["message"], "user promoted to admin");
    }

    /// GET /admin/users/{id}/keys with admin bearer → 200 with keys array.
    #[tokio::test]
    async fn test_admin_list_keys() {
        let app = build_router(test_state());
        let user_id = Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/admin/users/{user_id}/keys"))
            .header("Authorization", admin_bearer())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["keys"].is_array(), "response must have a 'keys' array");
    }

    /// POST /admin/users/{id}/demote where MockUserStore returns LastAdminDemotion → 400.
    #[tokio::test]
    async fn test_admin_demote_last_admin_rejected() {
        let app = build_router(test_state());
        let user_id = Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/admin/users/{user_id}/demote"))
            .header("Authorization", admin_bearer())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "demoting the last admin must return 400"
        );

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "bad_request");
        let msg = json["error"]["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("last admin"),
            "error message should mention 'last admin', got: {msg}"
        );
    }
}
