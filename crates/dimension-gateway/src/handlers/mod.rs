//! HTTP request handlers for the gateway API.
//!
//! - [`health`]: GET /health - liveness probe (no auth required)
//! - [`messages`]: POST /messages - accepts a message, returns SSE stream
//! - [`admin`]: /admin/* - admin user and key management (admin role required)
//! - [`sessions`]: /sessions/* - session management (auth required)
//! - [`bundles`]: bundle CRUD — upload, list, get, job status (auth required)
//! - [`bundle_jobs`]: GET /bundles/jobs/{id} - job status polling (auth required)
//! - [`secrets`]: /bundles/{id}/secrets, /tokenize, /detokenize - secrets management (auth required)
//! - [`storage`]: /bundles/{id}/storage/* - per-bundle object storage (auth + capability required)
//! - [`volumes`]: /volumes, /volumes/{id} - explicit volume management (auth required)
//! - [`artifacts`]: /sessions/{id}/artifacts, /artifacts - artifact browsing and download (auth required)
//! - [`logs`]: GET /admin/logs/tail - live SSE log streaming (admin required)
//! - [`invocations`]: GET /invocations/{id} - invocation metadata + optional log retrieval (auth required)

pub mod admin;
pub mod artifacts;
pub mod bundle_jobs;
pub mod bundles;
pub mod deployments;
pub mod health;
pub mod invocations;
pub mod logs;
pub mod run;
pub mod run_events;
pub mod secrets;
pub mod storage;
pub mod volumes;
pub mod workers;

pub use artifacts::{get_artifact_handler, list_artifacts_handler, list_user_artifacts_handler};

pub use bundle_jobs::get_job_handler;
pub use logs::logs_tail_handler;
pub use bundles::{get_bundle_handler, list_bundles_handler, push_handler, rollback_handler, upload_handler};
pub use health::health_handler;
pub use secrets::{
    create_secret_handler, delete_secret_handler, detokenize_handler, list_secrets_handler,
    tokenize_handler,
};
pub use storage::{
    delete_storage_handler, get_storage_handler, list_storage_handler, put_storage_handler,
};
