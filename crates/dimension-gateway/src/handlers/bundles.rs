//! Bundle HTTP handlers.
//!
//! - [`upload_handler`]: POST /bundles/upload - accept multipart Docker image tar, kick off async conversion
//! - [`push_handler`]: POST /bundles/push - accept pre-built ext4 rootfs directly (no Docker conversion)
//! - [`list_bundles_handler`]: GET /bundles - cursor-paginated bundle listing with owner filtering
//! - [`get_bundle_handler`]: GET /bundles/{id} - single bundle detail

use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read as IoRead, Write as IoWrite};
use uuid::Uuid;

use dimension_store::{AuthenticatedUser, UserRole};
use hyphae_core::registry::Registry;

use crate::bundle_security::validate_and_extract;
use crate::bundle_store::types::JobStage;
use crate::bundle_store::BundleJobStore;
use crate::models::error::AppError;
use crate::server::AppState;
use crate::worker::registry::WorkerRegistry;

/// Response body for POST /bundles/upload (202 Accepted).
#[derive(Debug, Serialize)]
pub struct UploadAccepted {
    pub job_id: Uuid,
    pub status: &'static str,
    pub poll_url: String,
}

/// Full bundle detail response (used by list and get endpoints).
#[derive(Debug, Serialize, Clone)]
pub struct BundleResponse {
    pub id: i64,
    pub name: String,
    pub tag: String,
    pub content_hash: String,
    /// None = platform bundle; per user decision: owner_id is the authenticated user's UUID.
    pub owner_id: Option<String>,
    pub size_bytes: u64,
    pub created_at: i64,
    pub default_vcpus: i64,
    pub default_memory_mib: i64,
}

/// Response body for GET /bundles (paginated list).
#[derive(Debug, Serialize)]
pub struct BundleListResponse {
    pub bundles: Vec<BundleResponse>,
    /// Cursor for the next page; None when there are no more results.
    pub next_cursor: Option<String>,
}

/// Query parameters for POST /bundles/upload.
#[derive(Debug, Deserialize)]
pub struct UploadParams {
    pub name: Option<String>,
    pub tag: Option<String>,
    /// When true, creates a platform bundle (owner_id = NULL).
    /// Only admins may set this; non-admins receive 403 Forbidden.
    pub platform: Option<bool>,
}

/// Query parameters for GET /bundles (cursor-based pagination).
#[derive(Debug, Deserialize)]
pub struct ListBundlesParams {
    /// Cursor value: created_at timestamp of the last item from the previous page.
    /// Results will have created_at <= cursor (since results are ordered DESC).
    pub cursor: Option<String>,
    /// Max items to return. Default 20, max 100.
    pub limit: Option<i64>,
}

/// POST /bundles/upload -- accept a Docker image tar, kick off async conversion.
///
/// The multipart body must contain a field named "image" with the tar file content.
/// Returns 202 Accepted with a job_id to poll via GET /bundles/jobs/{id}.
///
/// Owner assignment:
/// - Regular users: owner_id = authenticated user's user_id (private bundle)
/// - Admins with platform=true: owner_id = NULL (visible to all users)
pub async fn upload_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Query(params): Query<UploadParams>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadAccepted>), AppError> {
    // Determine owner_id based on the platform flag and user role.
    let owner_id: Option<String> = if params.platform.unwrap_or(false) {
        // Admin platform bundle upload — owner_id = NULL per user decision.
        if user.role != UserRole::Admin {
            return Err(AppError::Forbidden(
                "only admins can upload platform bundles".into(),
            ));
        }
        None // Platform bundle: owner_id = NULL
    } else {
        // Regular user upload — owner_id = authenticated user_id per user decision.
        Some(user.user_id.to_string())
    };

    // Extract the "image" field from the multipart body, streaming to a temp file.
    let mut tmp_file = tempfile::NamedTempFile::new()
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    const SIZE_LIMIT: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB
    let mut bytes_received: u64 = 0;
    let mut found_image_field = false;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("multipart error: {e}")))?
    {
        let field_name = field.name().unwrap_or("").to_string();
        if field_name != "image" {
            continue;
        }
        found_image_field = true;

        // Stream chunks to the temp file, enforcing the 2 GiB limit.
        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::BadRequest(format!("failed to read field: {e}")))?;

        bytes_received = bytes_received.saturating_add(data.len() as u64);
        if bytes_received > SIZE_LIMIT {
            return Err(AppError::PayloadTooLarge);
        }

        tmp_file
            .write_all(&data)
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        break; // Only read the first "image" field.
    }

    if !found_image_field {
        return Err(AppError::BadRequest("missing required field: image".into()));
    }

    let tmp_path = tmp_file.path().to_path_buf();

    // Compute SHA-256 content hash of the uploaded tar for dedup.
    let content_hash = {
        let mut file = std::fs::File::open(&tmp_path)
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 65536];
        loop {
            let n = file.read(&mut buf).map_err(|e| AppError::Internal(Box::new(e)))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hex::encode(hasher.finalize())
    };

    // Dedup: check if this content hash is already registered.
    let registry_path = state.registry_path.clone();
    let hash_clone = content_hash.clone();
    let dedup_result = tokio::task::spawn_blocking(move || {
        let registry = Registry::open(&registry_path)
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        registry
            .find_by_hash(&hash_clone)
            .map_err(|e| AppError::Internal(Box::new(e)))
    })
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))??;

    let job_id = Uuid::new_v4();
    state.bundle_job_store.create(job_id);

    if let Some(existing) = dedup_result {
        // Content hash hit — complete the job immediately without re-conversion.
        tracing::debug!(
            content_hash = %content_hash,
            bundle_id = existing.id,
            "bundle dedup hit — skipping conversion"
        );
        state.bundle_job_store.complete(job_id, existing.id);
    } else {
        // No dedup hit — spawn the async conversion pipeline.
        let job_store = state.bundle_job_store.clone();
        let registry_path = state.registry_path.clone();
        let upload_name = params.name;
        let upload_tag = params.tag.unwrap_or_else(|| "latest".to_string());
        let task_store = state.task_store.clone();
        // Build base_url from config for the agent card endpoint.
        let base_url = format!("http://{}:{}", state.config.host, state.config.port);
        let uploader_user_id = user.user_id;

        tokio::spawn(async move {
            run_conversion_pipeline(
                job_id,
                job_store,
                tmp_file,
                tmp_path,
                registry_path,
                upload_name,
                upload_tag,
                owner_id,
                task_store,
                base_url,
                uploader_user_id,
            )
            .await;
        });
    }

    let poll_url = format!("/bundles/jobs/{job_id}");
    Ok((
        StatusCode::ACCEPTED,
        Json(UploadAccepted {
            job_id,
            status: "queued",
            poll_url,
        }),
    ))
}

/// Response body for POST /bundles/push (200 OK — synchronous push).
#[derive(Debug, Serialize)]
pub struct PushResponse {
    /// The registered image ID in the registry.
    pub bundle_id: i64,
    /// SHA-256 content hash of the uploaded ext4 rootfs.
    pub content_hash: String,
    /// Human-readable status.
    pub status: &'static str,
}

/// POST /bundles/push -- accept a pre-built ext4 rootfs file directly.
///
/// Unlike `/bundles/upload` (which accepts a Docker tar and runs async conversion),
/// this endpoint accepts a ready-to-use ext4 rootfs, hashes it, stores it, and
/// registers it synchronously. Used by `dimension push` after `dimension build`.
///
/// Multipart fields:
/// - `file` (required): the ext4 rootfs binary
/// - `name` (required): bundle name
/// - `tag` (optional, defaults to "latest"): version tag
///
/// Version retention: after registration, enforces a limit of 3 versions per
/// bundle name+owner, garbage-collecting older versions.
pub async fn push_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    mut multipart: Multipart,
) -> Result<Json<PushResponse>, AppError> {
    let owner_id = user.user_id.to_string();

    // Collect multipart fields: file, name, tag, manifest.
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut bundle_name: Option<String> = None;
    let mut bundle_tag: Option<String> = None;
    let mut manifest_json: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("multipart error: {e}")))?
    {
        let field_name = field.name().unwrap_or("").to_string();
        match field_name.as_str() {
            "file" => {
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("failed to read file field: {e}")))?;

                const SIZE_LIMIT: usize = 2 * 1024 * 1024 * 1024; // 2 GiB
                if data.len() > SIZE_LIMIT {
                    return Err(AppError::PayloadTooLarge);
                }
                file_bytes = Some(data.to_vec());
            }
            "name" => {
                bundle_name = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::BadRequest(format!("failed to read name: {e}")))?,
                );
            }
            "tag" => {
                bundle_tag = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::BadRequest(format!("failed to read tag: {e}")))?,
                );
            }
            "manifest" => {
                manifest_json = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::BadRequest(format!("failed to read manifest: {e}")))?,
                );
            }
            _ => { /* ignore unknown fields */ }
        }
    }

    let file_data = file_bytes.ok_or_else(|| AppError::BadRequest("missing required field: file".into()))?;
    let name = bundle_name.ok_or_else(|| AppError::BadRequest("missing required field: name".into()))?;
    let tag = bundle_tag.unwrap_or_else(|| "latest".to_string());

    // Compute SHA-256 content hash.
    let content_hash = {
        let mut hasher = Sha256::new();
        hasher.update(&file_data);
        hex::encode(hasher.finalize())
    };

    let size_bytes = file_data.len() as u64;
    let registry_path = state.registry_path.clone();
    let hash_clone = content_hash.clone();
    let name_clone = name.clone();
    let tag_clone = tag.clone();
    let owner_clone = owner_id.clone();
    let manifest_clone = manifest_json.clone();

    // Parse manifest to extract resource defaults and per-section JSON.
    let manifest: Option<hyphae_core::manifest::DimensionManifest> = manifest_clone
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str(s).ok());

    let m_env = manifest.as_ref().and_then(|m| serde_json::to_string(&m.env).ok());
    let m_resources = manifest.as_ref().and_then(|m| serde_json::to_string(&m.resources).ok());
    let m_secrets = manifest.as_ref().and_then(|m| serde_json::to_string(&m.secrets).ok());
    let m_capabilities = manifest.as_ref().and_then(|m| serde_json::to_string(&m.capabilities).ok());
    let m_a2a = manifest.as_ref().and_then(|m| serde_json::to_string(&m.a2a).ok());
    let m_timeout = manifest.as_ref().and_then(|m| m.resources.timeout_secs.map(|t| t as i64));
    let m_volumes = manifest.as_ref().and_then(|m| serde_json::to_string(&m.volumes).ok());
    let m_memory = manifest.as_ref().and_then(|m| m.resources.memory_mb).unwrap_or(256) as i64;
    let m_vcpus = manifest.as_ref().and_then(|m| m.resources.vcpus).unwrap_or(2) as i64;

    // Run registry operations in spawn_blocking (rusqlite is !Send).
    let result = tokio::task::spawn_blocking(move || -> Result<PushResponse, AppError> {
        let registry = Registry::open(&registry_path)
            .map_err(|e| AppError::Internal(Box::new(e)))?;

        // Dedup: check if this content hash is already registered.
        if let Some(existing) = registry.find_by_hash(&hash_clone)
            .map_err(|e| AppError::Internal(Box::new(e)))?
        {
            return Ok(PushResponse {
                bundle_id: existing.id,
                content_hash: existing.content_hash,
                status: "already_exists",
            });
        }

        // Store the ext4 file to the registry storage dir.
        let disk_filename = format!("sha256-{}.ext4", hash_clone);
        let disk_path = registry.storage_dir().join(&disk_filename);
        std::fs::write(&disk_path, &file_data)
            .map_err(|e| AppError::Internal(Box::new(e)))?;

        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Register in the images table with manifest data from the CLI.
        let new_image = hyphae_core::registry::NewImage {
            content_hash: hash_clone.clone(),
            name: name_clone.clone(),
            tag: tag_clone,
            size_bytes,
            source_path: "push".to_string(),
            init_config: None,
            disk_path: disk_path.to_string_lossy().to_string(),
            created_at,
            default_vcpus: m_vcpus,
            default_memory_mib: m_memory,
            owner_id: Some(owner_clone.clone()),
            manifest_resources: m_resources,
            manifest_env: m_env,
            manifest_secrets: m_secrets,
            manifest_capabilities: m_capabilities,
            manifest_a2a: m_a2a,
            manifest_timeout_secs: m_timeout,
            manifest_volumes: m_volumes,
        };

        let registered = registry.register_image(&new_image)
            .map_err(|e| AppError::Internal(Box::new(e)))?;

        // Enforce version retention: keep last 3 versions.
        if let Err(e) = registry.enforce_version_limit(&name_clone, Some(&owner_clone), 3) {
            tracing::warn!(
                name = %name_clone,
                owner = %owner_clone,
                error = %e,
                "version limit enforcement failed (non-fatal)"
            );
        }

        Ok(PushResponse {
            bundle_id: registered.id,
            content_hash: hash_clone,
            status: "created",
        })
    })
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))??;

    // After successful push (not dedup), distribute to workers best-effort.
    if result.status == "created" {
        if let Some(ref wr) = state.worker_registry {
            let registry_path = state.registry_path.clone();
            let name = name.clone();
            let tag = tag.clone();
            let content_hash = result.content_hash.clone();
            let manifest_for_workers = manifest_json.unwrap_or_default();
            let wr = wr.clone();
            tokio::spawn(async move {
                distribute_to_workers(&wr, &registry_path, &name, &tag, &content_hash, &manifest_for_workers).await;
            });
        }
    }

    Ok(Json(result))
}

/// Response body for POST /bundles/{name}/rollback.
#[derive(Debug, Serialize)]
pub struct RollbackResponse {
    /// The newly-activated bundle version ID.
    pub bundle_id: i64,
    /// Content hash of the rolled-back version.
    pub content_hash: String,
    /// Human-readable status.
    pub status: &'static str,
}

/// POST /bundles/{name}/rollback — swap the active version to the previous one.
///
/// Looks up all versions for the bundle name + authenticated owner, picks the
/// second-newest (by `created_at`), and redistributes it to all workers.
/// Returns 400 if only one version exists (nothing to rollback to).
pub async fn rollback_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> Result<Json<RollbackResponse>, AppError> {
    let owner_id = user.user_id.to_string();
    let registry_path = state.registry_path.clone();
    let name_clone = name.clone();
    let owner_clone = owner_id.clone();

    let rollback_target = tokio::task::spawn_blocking(move || -> Result<hyphae_core::registry::image::ImageRecord, AppError> {
        let registry = Registry::open(&registry_path)
            .map_err(|e| AppError::Internal(Box::new(e)))?;

        let versions = registry.list_versions_by_name(&name_clone, Some(&owner_clone))
            .map_err(|e| AppError::Internal(Box::new(e)))?;

        if versions.len() < 2 {
            return Err(AppError::BadRequest(
                "cannot rollback: only one version exists".into(),
            ));
        }

        // versions are sorted by created_at DESC, so [0] is current, [1] is previous.
        Ok(versions[1].clone())
    })
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))??;

    // Distribute the rollback target to workers.
    // Rollback doesn't have the manifest handy — pass empty and let the worker
    // use whatever it already has stored for this content hash.
    if let Some(ref wr) = state.worker_registry {
        distribute_to_workers(
            wr,
            &state.registry_path,
            &name,
            &rollback_target.tag,
            &rollback_target.content_hash,
            "",
        )
        .await;
    }

    Ok(Json(RollbackResponse {
        bundle_id: rollback_target.id,
        content_hash: rollback_target.content_hash,
        status: "rolled_back",
    }))
}

/// Distribute a bundle ext4 to all registered workers via gRPC PushBundle RPC.
///
/// Best-effort: failures are logged but do not propagate. Each worker is contacted
/// concurrently using `tokio::spawn`.
async fn distribute_to_workers(
    worker_registry: &WorkerRegistry,
    registry_path: &std::path::Path,
    name: &str,
    tag: &str,
    content_hash: &str,
    manifest_json: &str,
) {
    use crate::worker::worker_proto::PushBundleRequest;

    let workers = worker_registry.get_all();
    if workers.is_empty() {
        tracing::debug!("no workers registered — skipping bundle distribution");
        return;
    }

    // Read the ext4 file from the registry storage dir.
    let disk_filename = format!("sha256-{}.ext4", content_hash);
    let disk_path = registry_path.join("images").join(&disk_filename);
    let ext4_data = match std::fs::read(&disk_path) {
        Ok(data) => data,
        Err(e) => {
            tracing::error!(
                path = %disk_path.display(),
                error = %e,
                "failed to read ext4 for distribution"
            );
            return;
        }
    };

    let size_bytes = ext4_data.len() as u64;
    let ext4_bytes = bytes::Bytes::from(ext4_data);

    tracing::info!(
        name = %name,
        content_hash = %content_hash,
        worker_count = workers.len(),
        "distributing bundle to workers"
    );

    let mut handles = Vec::new();
    for worker in workers {
        let mut client = worker.client.clone();
        let req = PushBundleRequest {
            name: name.to_string(),
            tag: tag.to_string(),
            content_hash: content_hash.to_string(),
            ext4_data: ext4_bytes.to_vec(),
            size_bytes,
            manifest_json: manifest_json.to_string(),
        };
        let worker_id = worker.worker_id;

        handles.push(tokio::spawn(async move {
            match client.push_bundle(tonic::Request::new(req)).await {
                Ok(resp) => {
                    let inner = resp.into_inner();
                    if inner.success {
                        tracing::info!(
                            worker_id = %worker_id,
                            bundle_id = inner.bundle_id,
                            "bundle distributed to worker"
                        );
                    } else {
                        tracing::warn!(
                            worker_id = %worker_id,
                            error = %inner.error,
                            "worker rejected bundle push"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        worker_id = %worker_id,
                        error = %e,
                        "failed to distribute bundle to worker"
                    );
                }
            }
        }));
    }

    // Wait for all distribution attempts to complete.
    for handle in handles {
        let _ = handle.await;
    }
}

/// Async bundle conversion pipeline: extract → build → register with owner_id.
///
/// This runs in a tokio::spawn task after upload_handler returns 202.
/// All errors are recorded in the job store; the temp file is cleaned up via RAII.
#[allow(clippy::too_many_arguments)]
async fn run_conversion_pipeline(
    job_id: Uuid,
    job_store: BundleJobStore,
    // NamedTempFile held here so it lives until the pipeline finishes (RAII drop).
    _tmp_file: tempfile::NamedTempFile,
    tmp_path: std::path::PathBuf,
    registry_path: std::path::PathBuf,
    upload_name: Option<String>,
    upload_tag: String,
    owner_id: Option<String>,
    _task_store: std::sync::Arc<dyn dimension_store::TaskStore>,
    _base_url: String,
    _uploader_user_id: Uuid,
) {
    use hyphae_core::orchestrator::Orchestrator;
    use hyphae_core::orchestrator::types::BuildRequest;

    // Stage 1 — Extracting
    job_store.set_stage(job_id, JobStage::Extracting, "validating and extracting archive");

    let staging_dir = match tempfile::TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            job_store.fail(job_id, JobStage::Extracting, format!("failed to create staging dir: {e}"));
            return;
        }
    };

    let staging_path = staging_dir.path().to_path_buf();
    let tar_path = tmp_path.clone();
    let extract_result = tokio::task::spawn_blocking(move || {
        validate_and_extract(&tar_path, &staging_path)
    })
    .await;

    match extract_result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            job_store.fail(job_id, JobStage::Extracting, format!("archive validation failed: {e}"));
            return;
        }
        Err(e) => {
            job_store.fail(job_id, JobStage::Extracting, format!("extraction task panicked: {e}"));
            return;
        }
    }

    // Stage 1.5 — Parse manifest (if present in the extracted bundle).
    // Must happen BEFORE Stage 2 (build) because we need the staging_dir path, and also
    // BEFORE the staging_dir is moved into the build closure.
    // Read dimension.toml from the staging directory; parse it if present.
    let manifest_path = staging_dir.path().join("dimension.toml");
    let manifest = if manifest_path.exists() {
        let content = match std::fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(e) => {
                job_store.fail(job_id, JobStage::Extracting, format!("failed to read dimension.toml: {e}"));
                return;
            }
        };
        match hyphae_core::manifest::parse_manifest(&content) {
            Ok(m) => Some(m),
            Err(e) => {
                // Invalid manifest TOML fails the conversion job with line/column info.
                job_store.fail(job_id, JobStage::Extracting, format!("invalid dimension.toml: {e}"));
                return;
            }
        }
    } else {
        // No manifest present -- safe defaults apply; manifest columns will be NULL.
        None
    };

    // Resolve manifest resource defaults (apply to BuildRequest if specified).
    let (default_vcpus, default_memory_mib) = manifest
        .as_ref()
        .and_then(|m| {
            let vcpus = m.resources.vcpus.map(|v| v as i64);
            let mem = m.resources.memory_mb.map(|m| m as i64);
            match (vcpus, mem) {
                (None, None) => None,
                (v, m) => Some((v.unwrap_or(2), m.unwrap_or(256))),
            }
        })
        .unwrap_or((2, 256));

    // Stage 2 — Converting
    job_store.set_stage(job_id, JobStage::Converting, "running build pipeline");

    let tmp_path_clone = tmp_path.clone();
    let registry_path_clone = registry_path.clone();
    let owner_id_clone = owner_id.clone();
    // Extract path from TempDir; the TempDir is moved into spawn_blocking to keep it alive.
    let staging_dir_path = staging_dir.path().to_path_buf();
    let build_result = tokio::task::spawn_blocking(move || {
        // Create a fresh Registry per upload (rusqlite is !Send).
        let registry = Registry::open(&registry_path_clone)
            .map_err(|e| format!("failed to open registry: {e}"))?;
        let orchestrator = Orchestrator::new(registry);

        let build_request = BuildRequest {
            project_dir: staging_dir_path,
            docker_image: Some(tmp_path_clone.to_string_lossy().to_string()),
            name: upload_name,
            tag: upload_tag,
            force: false,
            // Apply manifest resource defaults if declared; fall back to server defaults.
            default_vcpus,
            default_memory_mib,
            embed_dimension_agent: true,
            binary_path: None,
            entrypoint: None,
        };

        let result = orchestrator
            .build(build_request, None)
            .map_err(|e| format!("build failed: {e}"))?;

        // Stage 3 — Registering: persist owner_id and manifest data on the registered image.
        // Orchestrator::build registers the image; we update extra fields via post-build calls.
        {
            let registry2 = Registry::open(&registry_path_clone)
                .map_err(|e| format!("failed to re-open registry for owner_id update: {e}"))?;
            registry2
                .set_owner(result.image_id, owner_id_clone.as_deref())
                .map_err(|e| format!("failed to set owner_id: {e}"))?;
        }

        Ok::<_, String>((result.image_id, manifest))
    })
    .await;

    match build_result {
        Ok(Ok((image_id, manifest_opt))) => {
            // Persist manifest data (JSON columns) after successful build.
            // Uses a fresh registry connection to avoid borrow issues.
            if let Err(e) = (|| -> Result<(), String> {
                let registry3 = Registry::open(&registry_path)
                    .map_err(|e| format!("failed to open registry for manifest update: {e}"))?;

                let (res_json, env_json, sec_json, cap_json, a2a_json, timeout_secs, vol_json) =
                    match manifest_opt {
                        Some(ref m) => {
                            let res = serde_json::to_string(&m.resources)
                                .map_err(|e| format!("failed to serialize manifest_resources: {e}"))?;
                            let env = serde_json::to_string(&m.env.vars)
                                .map_err(|e| format!("failed to serialize manifest_env: {e}"))?;
                            let sec = serde_json::to_string(&m.secrets)
                                .map_err(|e| format!("failed to serialize manifest_secrets: {e}"))?;
                            let cap = serde_json::to_string(&m.capabilities)
                                .map_err(|e| format!("failed to serialize manifest_capabilities: {e}"))?;
                            let a2a = serde_json::to_string(&m.a2a)
                                .map_err(|e| format!("failed to serialize manifest_a2a: {e}"))?;
                            let timeout = m.resources.timeout_secs.map(|s| s as i64);
                            let vol = serde_json::to_string(&m.volumes)
                                .map_err(|e| format!("failed to serialize manifest_volumes: {e}"))?;
                            (Some(res), Some(env), Some(sec), Some(cap), Some(a2a), timeout, Some(vol))
                        }
                        None => (None, None, None, None, None, None, None),
                    };

                registry3
                    .update_manifest(
                        image_id,
                        res_json.as_deref(),
                        env_json.as_deref(),
                        sec_json.as_deref(),
                        cap_json.as_deref(),
                        a2a_json.as_deref(),
                        timeout_secs,
                        vol_json.as_deref(),
                    )
                    .map_err(|e| format!("failed to update manifest columns: {e}"))?;

                Ok(())
            })() {
                tracing::warn!(image_id, error = %e, "manifest columns could not be persisted (non-fatal)");
            }

            job_store.set_stage(job_id, JobStage::Registering, "bundle registered");
            job_store.complete(job_id, image_id);
        }
        Ok(Err(e)) => {
            job_store.fail(job_id, JobStage::Converting, e);
        }
        Err(e) => {
            job_store.fail(job_id, JobStage::Converting, format!("build task panicked: {e}"));
        }
    }
}

/// GET /bundles -- list bundles with cursor-based pagination.
///
/// Uses cursor from query params (created_at timestamp of the last returned item).
/// Returns bundles visible to the authenticated user plus a next_cursor for pagination.
pub async fn list_bundles_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Query(params): Query<ListBundlesParams>,
) -> Result<Json<BundleListResponse>, AppError> {
    let cursor_ts: Option<i64> = params
        .cursor
        .as_deref()
        .and_then(|c| c.parse().ok());
    let limit = params.limit.unwrap_or(20).min(100);

    let user_id_str = user.user_id.to_string();
    let is_admin = user.role == UserRole::Admin;
    let registry_path = state.registry_path.clone();

    // Registry uses synchronous rusqlite -- must call via spawn_blocking.
    let all_images = tokio::task::spawn_blocking(move || {
        let registry = Registry::open(&registry_path)
            .map_err(|e| AppError::Internal(Box::new(e)))?;

        if is_admin {
            // Admins see all bundles, with cursor-based pagination.
            registry
                .list_images(None, None, None, cursor_ts)
                .map_err(|e| AppError::Internal(Box::new(e)))
        } else {
            // Users see platform bundles + own bundles, filtered by cursor.
            // list_images_for_user doesn't support cursor, so we use list_images
            // and filter in-process for users.
            registry
                .list_images_for_user(Some(&user_id_str), false)
                .map_err(|e| AppError::Internal(Box::new(e)))
                .map(|images| {
                    if let Some(cursor) = cursor_ts {
                        images.into_iter().filter(|img| img.created_at <= cursor).collect()
                    } else {
                        images
                    }
                })
        }
    })
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))??;

    let has_more = all_images.len() > limit as usize;
    let page: Vec<_> = all_images.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        page.last().map(|img| img.created_at.to_string())
    } else {
        None
    };

    let bundles: Vec<BundleResponse> = page
        .into_iter()
        .map(|img| BundleResponse {
            id: img.id,
            name: img.name,
            tag: img.tag,
            content_hash: img.content_hash,
            owner_id: img.owner_id,
            size_bytes: img.size_bytes,
            created_at: img.created_at,
            default_vcpus: img.default_vcpus,
            default_memory_mib: img.default_memory_mib,
        })
        .collect();

    Ok(Json(BundleListResponse {
        bundles,
        next_cursor,
    }))
}

/// GET /bundles/{id} -- single bundle detail.
///
/// Returns the bundle including owner_id, or 404 if not found.
/// Ownership: platform bundles (owner_id=NULL) visible to all; user bundles only to owner or admin.
pub async fn get_bundle_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(id): Path<i64>,
) -> Result<Json<BundleResponse>, AppError> {
    let registry_path = state.registry_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        let registry = Registry::open(&registry_path)
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        registry
            .find_by_id(id)
            .map_err(|e| AppError::Internal(Box::new(e)))
    })
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))??;

    let img = result.ok_or_else(|| AppError::NotFound("bundle not found".into()))?;

    // Ownership: platform bundles (owner_id=NULL) visible to all; user bundles only to owner or admin
    if user.role != UserRole::Admin {
        if let Some(ref owner) = img.owner_id {
            if *owner != user.user_id.to_string() {
                return Err(AppError::Forbidden("bundle not found".into()));
            }
        }
    }

    Ok(Json(BundleResponse {
        id: img.id,
        name: img.name,
        tag: img.tag,
        content_hash: img.content_hash,
        owner_id: img.owner_id,
        size_bytes: img.size_bytes,
        created_at: img.created_at,
        default_vcpus: img.default_vcpus,
        default_memory_mib: img.default_memory_mib,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use axum::Router;
    use dimension_store::{AuthenticatedUser, UserRole};
    use hyphae_core::registry::image::NewImage;
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

    fn make_app_state(registry_path: std::path::PathBuf) -> AppState {
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

    fn make_app_with_user(
        registry_path: std::path::PathBuf,
        user: AuthenticatedUser,
    ) -> Router {
        let state = make_app_state(registry_path);
        Router::new()
            .route("/bundles/upload", post(upload_handler))
            .route("/bundles/push", post(push_handler))
            .route("/bundles", get(list_bundles_handler))
            .route("/bundles/{id}", get(get_bundle_handler))
            .route("/bundles/{name}/rollback", post(rollback_handler))
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

    fn make_admin_user() -> AuthenticatedUser {
        AuthenticatedUser {
            user_id: Uuid::new_v4(),
            name: "admin".into(),
            role: UserRole::Admin,
        }
    }

    fn make_regular_user() -> AuthenticatedUser {
        AuthenticatedUser {
            user_id: Uuid::new_v4(),
            name: "user".into(),
            role: UserRole::User,
        }
    }

    // --- list_bundles_handler tests ---

    /// GET /bundles returns 200 with { bundles: [...], next_cursor: ... } shape.
    #[tokio::test]
    async fn test_list_bundles_returns_200_with_correct_shape() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();

        registry
            .register_image(&NewImage {
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
                owner_id: None,
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            })
            .unwrap();

        let app = make_app_with_user(dir.path().to_path_buf(), make_admin_user());
        let req = Request::builder()
            .method("GET")
            .uri("/bundles")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert!(json["bundles"].is_array(), "bundles must be array");
        assert!(json.get("next_cursor").is_some(), "next_cursor field must be present");
        let bundles = json["bundles"].as_array().unwrap();
        assert_eq!(bundles.len(), 1);
        // Verify BundleResponse includes content_hash and owner_id fields
        assert!(bundles[0]["content_hash"].is_string());
        assert!(bundles[0]["size_bytes"].is_number());
    }

    /// GET /bundles with limit=1 returns next_cursor when more results exist.
    #[tokio::test]
    async fn test_list_bundles_pagination_with_limit() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();

        // Insert 2 bundles with distinct timestamps.
        registry
            .register_image(&NewImage {
                content_hash: "hash-a".into(),
                name: "app-a".into(),
                tag: "v1".into(),
                size_bytes: 1024,
                source_path: "/tmp/src".into(),
                init_config: None,
                disk_path: "/tmp/a.ext4".into(),
                created_at: 2000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: None,
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            })
            .unwrap();
        registry
            .register_image(&NewImage {
                content_hash: "hash-b".into(),
                name: "app-b".into(),
                tag: "v1".into(),
                size_bytes: 2048,
                source_path: "/tmp/src".into(),
                init_config: None,
                disk_path: "/tmp/b.ext4".into(),
                created_at: 1000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: None,
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            })
            .unwrap();

        let app = make_app_with_user(dir.path().to_path_buf(), make_admin_user());
        let req = Request::builder()
            .method("GET")
            .uri("/bundles?limit=1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let bundles = json["bundles"].as_array().unwrap();
        assert_eq!(bundles.len(), 1, "limit=1 should return exactly 1 bundle");
        assert!(
            json["next_cursor"].is_string(),
            "next_cursor must be a non-null string when more results exist"
        );
    }

    // --- get_bundle_handler tests ---

    /// GET /bundles/{id} returns bundle with owner_id field.
    #[tokio::test]
    async fn test_get_bundle_returns_owner_id() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();

        let user_id = Uuid::new_v4().to_string();
        let img = registry
            .register_image(&NewImage {
                content_hash: "owned-hash".into(),
                name: "owned-app".into(),
                tag: "v1".into(),
                size_bytes: 512,
                source_path: "/tmp/src".into(),
                init_config: None,
                disk_path: "/tmp/owned.ext4".into(),
                created_at: 5000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: Some(user_id.clone()),
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            })
            .unwrap();

        let app = make_app_with_user(dir.path().to_path_buf(), make_admin_user());
        let req = Request::builder()
            .method("GET")
            .uri(format!("/bundles/{}", img.id))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["id"], img.id);
        assert_eq!(json["owner_id"], user_id);
        assert_eq!(json["name"], "owned-app");
    }

    /// GET /bundles/{id} returns 403 when a non-admin user requests a bundle they do not own.
    #[tokio::test]
    async fn test_get_bundle_non_owner_returns_403() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();

        let owner_id = Uuid::new_v4();
        let img = registry
            .register_image(&NewImage {
                content_hash: "owned-hash-403".into(),
                name: "owned-app".into(),
                tag: "v1".into(),
                size_bytes: 512,
                source_path: "/tmp/src".into(),
                init_config: None,
                disk_path: "/tmp/owned-403.ext4".into(),
                created_at: 5000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: Some(owner_id.to_string()),
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            })
            .unwrap();

        // Request as a different user (not the owner)
        let other_user = AuthenticatedUser {
            user_id: Uuid::new_v4(),
            name: "other".into(),
            role: UserRole::User,
        };
        let app = make_app_with_user(dir.path().to_path_buf(), other_user);
        let req = Request::builder()
            .method("GET")
            .uri(format!("/bundles/{}", img.id))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// GET /bundles/{id} returns 200 for a bundle owner requesting their own bundle.
    #[tokio::test]
    async fn test_get_bundle_owner_returns_200() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();

        let owner_id = Uuid::new_v4();
        let img = registry
            .register_image(&NewImage {
                content_hash: "owner-hash-200".into(),
                name: "owner-app".into(),
                tag: "v1".into(),
                size_bytes: 512,
                source_path: "/tmp/src".into(),
                init_config: None,
                disk_path: "/tmp/owner-200.ext4".into(),
                created_at: 5000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: Some(owner_id.to_string()),
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            })
            .unwrap();

        // Request as the owner
        let owner_user = AuthenticatedUser {
            user_id: owner_id,
            name: "owner".into(),
            role: UserRole::User,
        };
        let app = make_app_with_user(dir.path().to_path_buf(), owner_user);
        let req = Request::builder()
            .method("GET")
            .uri(format!("/bundles/{}", img.id))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["id"], img.id);
        assert_eq!(json["name"], "owner-app");
    }

    /// GET /bundles/{id} returns 200 for an admin requesting any bundle.
    #[tokio::test]
    async fn test_get_bundle_admin_returns_200() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();

        let owner_id = Uuid::new_v4();
        let img = registry
            .register_image(&NewImage {
                content_hash: "admin-hash-200".into(),
                name: "admin-app".into(),
                tag: "v1".into(),
                size_bytes: 512,
                source_path: "/tmp/src".into(),
                init_config: None,
                disk_path: "/tmp/admin-200.ext4".into(),
                created_at: 5000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: Some(owner_id.to_string()),
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            })
            .unwrap();

        // Request as admin (different UUID from owner)
        let app = make_app_with_user(dir.path().to_path_buf(), make_admin_user());
        let req = Request::builder()
            .method("GET")
            .uri(format!("/bundles/{}", img.id))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["id"], img.id);
        assert_eq!(json["name"], "admin-app");
    }

    /// GET /bundles/{id} returns 200 for any user requesting a platform bundle (owner_id = NULL).
    #[tokio::test]
    async fn test_get_platform_bundle_returns_200() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();

        let img = registry
            .register_image(&NewImage {
                content_hash: "platform-hash-200".into(),
                name: "platform-app".into(),
                tag: "v1".into(),
                size_bytes: 512,
                source_path: "/tmp/src".into(),
                init_config: None,
                disk_path: "/tmp/platform-200.ext4".into(),
                created_at: 5000,
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
            })
            .unwrap();

        // Request as a regular user (not owner, but platform bundle is visible to all)
        let app = make_app_with_user(dir.path().to_path_buf(), make_regular_user());
        let req = Request::builder()
            .method("GET")
            .uri(format!("/bundles/{}", img.id))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["id"], img.id);
        assert_eq!(json["name"], "platform-app");
        assert!(json["owner_id"].is_null(), "platform bundle owner_id should be null");
    }

    /// GET /bundles/{unknown_id} returns 404.
    #[tokio::test]
    async fn test_get_bundle_unknown_id_returns_404() {
        let dir = tempfile::tempdir().unwrap();

        let app = make_app_with_user(dir.path().to_path_buf(), make_admin_user());
        let req = Request::builder()
            .method("GET")
            .uri("/bundles/99999")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "not_found");
    }

    // --- upload_handler tests ---

    fn build_multipart_body(boundary: &str, field_name: &str, data: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        let header = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{field_name}\"; filename=\"image.tar\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        );
        body.extend_from_slice(header.as_bytes());
        body.extend_from_slice(data);
        let footer = format!("\r\n--{boundary}--\r\n");
        body.extend_from_slice(footer.as_bytes());
        body
    }

    /// POST /bundles/upload with platform=true and non-admin user returns 403.
    #[tokio::test]
    async fn test_upload_platform_non_admin_returns_403() {
        let dir = tempfile::tempdir().unwrap();
        let user = make_regular_user();
        let app = make_app_with_user(dir.path().to_path_buf(), user);

        let boundary = "testboundary12345";
        let body = build_multipart_body(boundary, "image", b"fake tar data");

        let req = Request::builder()
            .method("POST")
            .uri("/bundles/upload?platform=true")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "forbidden");
    }

    /// POST /bundles/upload with platform=true and admin user returns 202.
    #[tokio::test]
    async fn test_upload_platform_admin_returns_202() {
        let dir = tempfile::tempdir().unwrap();
        let user = make_admin_user();
        let app = make_app_with_user(dir.path().to_path_buf(), user);

        let boundary = "testboundary12345";
        let body = build_multipart_body(boundary, "image", b"fake tar content for platform");

        let req = Request::builder()
            .method("POST")
            .uri("/bundles/upload?platform=true")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert!(json["job_id"].is_string(), "job_id must be present");
        assert_eq!(json["status"], "queued");
        assert!(json["poll_url"].is_string(), "poll_url must be present");
    }

    /// POST /bundles/upload with missing "image" field returns 400.
    #[tokio::test]
    async fn test_upload_missing_image_field_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let user = make_admin_user();
        let app = make_app_with_user(dir.path().to_path_buf(), user);

        let boundary = "testboundary12345";
        // Send the wrong field name.
        let body = build_multipart_body(boundary, "not-image", b"some data");

        let req = Request::builder()
            .method("POST")
            .uri("/bundles/upload")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "bad_request");
    }

    /// POST /bundles/upload with regular user (no platform flag) returns 202.
    #[tokio::test]
    async fn test_upload_regular_user_returns_202() {
        let dir = tempfile::tempdir().unwrap();
        let user = make_regular_user();
        let app = make_app_with_user(dir.path().to_path_buf(), user);

        let boundary = "testboundary12345";
        let body = build_multipart_body(boundary, "image", b"user tar data");

        let req = Request::builder()
            .method("POST")
            .uri("/bundles/upload")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["job_id"].is_string());
        assert_eq!(json["status"], "queued");
    }

    // --- push_handler tests ---

    /// Build a multipart body for POST /bundles/push with file, name, and tag fields.
    fn build_push_multipart(boundary: &str, file_data: &[u8], name: &str, tag: &str) -> Vec<u8> {
        let mut body = Vec::new();
        // file field
        body.extend_from_slice(format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"bundle.ext4\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        ).as_bytes());
        body.extend_from_slice(file_data);
        // name field
        body.extend_from_slice(format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\n{name}"
        ).as_bytes());
        // tag field
        body.extend_from_slice(format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"tag\"\r\n\r\n{tag}"
        ).as_bytes());
        // terminator
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    }

    /// POST /bundles/push with valid ext4 data returns 200 with bundle metadata.
    #[tokio::test]
    async fn test_push_handler_returns_200_with_bundle_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let user = make_regular_user();
        let app = make_app_with_user(dir.path().to_path_buf(), user);

        let boundary = "pushboundary123";
        let body = build_push_multipart(boundary, b"fake ext4 rootfs data", "test-push-app", "v1");

        let req = Request::builder()
            .method("POST")
            .uri("/bundles/push")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert!(json["bundle_id"].is_number(), "bundle_id must be a number");
        assert!(json["content_hash"].is_string(), "content_hash must be a string");
        assert_eq!(json["status"], "created");
    }

    /// POST /bundles/push with duplicate content returns "already_exists".
    #[tokio::test]
    async fn test_push_handler_dedup_returns_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let user = make_regular_user();
        let boundary = "pushboundary456";
        let data = b"duplicate ext4 content";

        // First push — should return "created".
        let app1 = make_app_with_user(dir.path().to_path_buf(), user.clone());
        let body1 = build_push_multipart(boundary, data, "dedup-app", "v1");
        let req1 = Request::builder()
            .method("POST")
            .uri("/bundles/push")
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .body(Body::from(body1))
            .unwrap();
        let resp1 = app1.oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), StatusCode::OK);
        let bytes1 = resp1.into_body().collect().await.unwrap().to_bytes();
        let json1: serde_json::Value = serde_json::from_slice(&bytes1).unwrap();
        assert_eq!(json1["status"], "created");

        // Second push with same content — should return "already_exists".
        let app2 = make_app_with_user(dir.path().to_path_buf(), user);
        let body2 = build_push_multipart(boundary, data, "dedup-app", "v2");
        let req2 = Request::builder()
            .method("POST")
            .uri("/bundles/push")
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .body(Body::from(body2))
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);
        let bytes2 = resp2.into_body().collect().await.unwrap().to_bytes();
        let json2: serde_json::Value = serde_json::from_slice(&bytes2).unwrap();
        assert_eq!(json2["status"], "already_exists");
    }

    /// POST /bundles/push without file field returns 400.
    #[tokio::test]
    async fn test_push_handler_missing_file_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let user = make_regular_user();
        let app = make_app_with_user(dir.path().to_path_buf(), user);

        // Build multipart with only name field, no file
        let boundary = "pushboundary789";
        let mut body = Vec::new();
        body.extend_from_slice(format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nmy-app"
        ).as_bytes());
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let req = Request::builder()
            .method("POST")
            .uri("/bundles/push")
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// POST /bundles/push without name field returns 400.
    #[tokio::test]
    async fn test_push_handler_missing_name_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let user = make_regular_user();
        let app = make_app_with_user(dir.path().to_path_buf(), user);

        // Build multipart with only file field, no name
        let boundary = "pushboundary000";
        let body = build_multipart_body(boundary, "file", b"some ext4 data");

        let req = Request::builder()
            .method("POST")
            .uri("/bundles/push")
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // --- rollback_handler tests ---

    /// POST /bundles/{name}/rollback with two versions returns the previous version.
    #[tokio::test]
    async fn test_rollback_returns_previous_version() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();
        let user = make_regular_user();
        let owner_id = user.user_id.to_string();

        // Register two versions with distinct timestamps.
        let _v1 = registry
            .register_image(&NewImage {
                content_hash: "rollback-hash-v1".into(),
                name: "rollback-app".into(),
                tag: "v1".into(),
                size_bytes: 1024,
                source_path: "/tmp/src".into(),
                init_config: None,
                disk_path: "/tmp/rb-v1.ext4".into(),
                created_at: 1000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: Some(owner_id.clone()),
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            })
            .unwrap();

        let _v2 = registry
            .register_image(&NewImage {
                content_hash: "rollback-hash-v2".into(),
                name: "rollback-app".into(),
                tag: "v2".into(),
                size_bytes: 2048,
                source_path: "/tmp/src".into(),
                init_config: None,
                disk_path: "/tmp/rb-v2.ext4".into(),
                created_at: 2000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: Some(owner_id.clone()),
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            })
            .unwrap();

        let app = make_app_with_user(dir.path().to_path_buf(), user);
        let req = Request::builder()
            .method("POST")
            .uri("/bundles/rollback-app/rollback")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["status"], "rolled_back");
        assert_eq!(json["content_hash"], "rollback-hash-v1");
        assert!(json["bundle_id"].is_number());
    }

    /// POST /bundles/{name}/rollback with only one version returns 400.
    #[tokio::test]
    async fn test_rollback_single_version_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();
        let user = make_regular_user();
        let owner_id = user.user_id.to_string();

        registry
            .register_image(&NewImage {
                content_hash: "solo-hash".into(),
                name: "solo-app".into(),
                tag: "v1".into(),
                size_bytes: 1024,
                source_path: "/tmp/src".into(),
                init_config: None,
                disk_path: "/tmp/solo.ext4".into(),
                created_at: 1000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: Some(owner_id),
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            })
            .unwrap();

        let app = make_app_with_user(dir.path().to_path_buf(), user);
        let req = Request::builder()
            .method("POST")
            .uri("/bundles/solo-app/rollback")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// POST /bundles/{name}/rollback with no versions returns 400.
    #[tokio::test]
    async fn test_rollback_no_versions_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let user = make_regular_user();

        let app = make_app_with_user(dir.path().to_path_buf(), user);
        let req = Request::builder()
            .method("POST")
            .uri("/bundles/nonexistent-app/rollback")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Rollback selects the second-newest version when three exist.
    #[tokio::test]
    async fn test_rollback_selects_second_newest() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();
        let user = make_regular_user();
        let owner_id = user.user_id.to_string();

        for (i, ts) in [(1, 1000), (2, 2000), (3, 3000)] {
            registry
                .register_image(&NewImage {
                    content_hash: format!("multi-hash-v{}", i),
                    name: "multi-app".into(),
                    tag: format!("v{}", i),
                    size_bytes: 1024,
                    source_path: "/tmp/src".into(),
                    init_config: None,
                    disk_path: format!("/tmp/multi-v{}.ext4", i),
                    created_at: ts,
                    default_vcpus: 2,
                    default_memory_mib: 256,
                    owner_id: Some(owner_id.clone()),
                    manifest_resources: None,
                    manifest_env: None,
                    manifest_secrets: None,
                    manifest_capabilities: None,
                    manifest_a2a: None,
                    manifest_timeout_secs: None,
                    manifest_volumes: None,
                })
                .unwrap();
        }

        let app = make_app_with_user(dir.path().to_path_buf(), user);
        let req = Request::builder()
            .method("POST")
            .uri("/bundles/multi-app/rollback")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        // v3 (created_at=3000) is current, v2 (created_at=2000) is the rollback target
        assert_eq!(json["content_hash"], "multi-hash-v2");
        assert_eq!(json["status"], "rolled_back");
    }
}
