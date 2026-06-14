//! Build pipeline: project directory -> rootfs image -> registered bundle.
//!
//! The build pipeline detects project type, checks the content-addressed
//! cache, builds a rootfs image, and registers it in the registry.

use std::time::{SystemTime, UNIX_EPOCH};

use hyphae_errors::{HyphaeError, OrchestratorError};
use serde_json;

use crate::registry::hash::hash_source_directory;
use crate::registry::image::NewImage;
use crate::registry::storage::image_file_path;
use crate::registry::{derive_image_name, parse_image_ref};
use crate::rootfs::{BuildConfig, build_rootfs, detect_project_type};

use super::types::{
    ActionType, BuildPlan, BuildRequest, BuildResult, CacheStatus, PlannedAction, ProgressFn,
};
use super::Orchestrator;

impl Orchestrator {
    /// Execute the build pipeline.
    ///
    /// Steps:
    /// 1. Validate project directory exists
    /// 2. Derive image name if not provided
    /// 3. Detect project type
    /// 4. Hash source directory for content addressing
    /// 5. Check cache (unless force rebuild)
    /// 6. Build rootfs image
    /// 7. Register in registry
    /// 8. Return BuildResult
    pub fn build(
        &self,
        request: BuildRequest,
        progress: Option<ProgressFn>,
    ) -> Result<BuildResult, HyphaeError> {
        let emit = |msg: &str| {
            if let Some(ref cb) = progress {
                cb(msg);
            }
        };

        let is_binary_mode = request.binary_path.is_some();
        let is_docker_mode = request.docker_image.is_some();

        // 1. Validate source path exists.
        if is_docker_mode {
            // Docker mode: no local path to validate
        } else if is_binary_mode {
            let bp = request.binary_path.as_ref().unwrap();
            if !bp.exists() {
                return Err(OrchestratorError::ProjectDirNotFound {
                    path: bp.clone(),
                }
                .into());
            }
        } else if !request.project_dir.exists() {
            return Err(OrchestratorError::ProjectDirNotFound {
                path: request.project_dir.clone(),
            }
            .into());
        }

        // 2. Derive image name.
        let name = match &request.name {
            Some(n) => n.clone(),
            None => {
                if is_docker_mode {
                    // Extract name from docker image ref, e.g. "nginx:1.25" -> "nginx"
                    let img = request.docker_image.as_ref().unwrap();
                    let without_tag = img.split(':').next().unwrap_or(img);
                    let short = without_tag.rsplit('/').next().unwrap_or(without_tag);
                    short.to_string()
                } else if is_binary_mode {
                    let bp = request.binary_path.as_ref().unwrap();
                    bp.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "binary".to_string())
                } else {
                    derive_image_name(&request.project_dir)
                        .map_err(|e| OrchestratorError::BuildFailed(e.to_string()))?
                }
            }
        };
        let tag = &request.tag;
        let reference = if tag == "latest" {
            name.clone()
        } else {
            format!("{name}:{tag}")
        };

        if is_docker_mode {
            emit(&format!(
                "docker mode: converting {}",
                request.docker_image.as_ref().unwrap()
            ));
        } else if is_binary_mode {
            emit(&format!(
                "binary mode: packaging {}",
                request.binary_path.as_ref().unwrap().display()
            ));
        } else {
            emit(&format!("detecting project type in {}", request.project_dir.display()));

            // 3. Detect project type.
            let project_type = detect_project_type(&request.project_dir)?;
            emit(&format!("detected project type: {project_type:?}"));
        }

        // 4. Hash source.
        // Docker tags are mutable, so we use the image ref as a simple hash key.
        // This means docker builds always get a consistent hash for the same ref,
        // but users should use --force if the image content has changed.
        let content_hash = if is_docker_mode {
            let img = request.docker_image.as_ref().unwrap();
            use sha2::{Sha256, Digest};
            let hash = Sha256::digest(img.as_bytes());
            let h = hex::encode(hash);
            emit(&format!("content hash (docker ref): {h}"));
            h
        } else {
            let hash_path = if is_binary_mode {
                let bp = request.binary_path.as_ref().unwrap();
                emit("hashing binary for cache check");
                if bp.is_file() {
                    bp.parent().unwrap_or(bp).to_path_buf()
                } else {
                    bp.clone()
                }
            } else {
                emit("hashing source directory for cache check");
                request.project_dir.clone()
            };
            let h = hash_source_directory(&hash_path)
                .map_err(|e| OrchestratorError::BuildFailed(e.to_string()))?;
            emit(&format!("content hash: {h}"));
            h
        };

        // 5. Check cache (unless force).
        if !request.force {
            if let Some(existing) = self
                .registry
                .find_by_hash(&content_hash)
                .map_err(|e| OrchestratorError::RegistryFailed(e.to_string()))?
            {
                emit("cache hit -- reusing existing image");
                return Ok(BuildResult {
                    reference,
                    content_hash,
                    disk_path: existing.disk_path,
                    image_size: existing.size_bytes,
                    cached: true,
                    image_id: existing.id,
                });
            }
        }

        // 6. Build rootfs image.
        emit("building rootfs image");
        let output_path = image_file_path(self.registry.storage_dir(), &content_hash);

        let build_config = BuildConfig {
            project_dir: request.project_dir.clone(),
            output_path: output_path.clone(),
            size_override: None,
            embed_dimension_agent: request.embed_dimension_agent,
            binary_path: request.binary_path.clone(),
            entrypoint: request.entrypoint.clone(),
            docker_image: request.docker_image.clone(),
        };

        let rootfs_result = build_rootfs(&build_config)?;
        emit(&format!(
            "rootfs built: {} bytes ({} content)",
            rootfs_result.image_size, rootfs_result.content_size
        ));

        // 7. Register in registry.
        emit("registering image in registry");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let new_image = NewImage {
            content_hash: content_hash.clone(),
            name,
            tag: tag.clone(),
            size_bytes: rootfs_result.image_size,
            source_path: if is_docker_mode {
                format!("docker:{}", request.docker_image.as_ref().unwrap())
            } else if is_binary_mode {
                request.binary_path.as_ref().unwrap().to_string_lossy().to_string()
            } else {
                request.project_dir.to_string_lossy().to_string()
            },
            init_config: None,
            disk_path: output_path.to_string_lossy().to_string(),
            created_at: now,
            default_vcpus: request.default_vcpus,
            default_memory_mib: request.default_memory_mib,
            owner_id: None, // Platform bundle by default -- callers can set user ownership
            // Manifest columns are set to None here; the upload pipeline stores
            // manifest data via Registry::update_manifest() after build completes.
            manifest_resources: None,
            manifest_env: None,
            manifest_secrets: None,
            manifest_capabilities: None,
            manifest_a2a: None,
            manifest_timeout_secs: None,
            manifest_volumes: None,
        };

        let registered = self
            .registry
            .register_image(&new_image)
            .map_err(|e| OrchestratorError::RegistryFailed(e.to_string()))?;

        // If a manifest was parsed from dimension.toml in the rootfs, update the registry
        if let Some(ref manifest) = rootfs_result.manifest {
            let resources = serde_json::to_string(&manifest.resources).ok();
            let env = serde_json::to_string(&manifest.env).ok();
            let secrets = serde_json::to_string(&manifest.secrets).ok();
            let capabilities = serde_json::to_string(&manifest.capabilities).ok();
            let a2a = serde_json::to_string(&manifest.a2a).ok();
            let timeout_secs = manifest.resources.timeout_secs.map(|t| t as i64);
            let volumes = serde_json::to_string(&manifest.volumes).ok();

            self.registry
                .update_manifest(
                    registered.id,
                    resources.as_deref(),
                    env.as_deref(),
                    secrets.as_deref(),
                    capabilities.as_deref(),
                    a2a.as_deref(),
                    timeout_secs,
                    volumes.as_deref(),
                )
                .map_err(|e| OrchestratorError::RegistryFailed(e.to_string()))?;
            emit("manifest stored in registry");
        }

        emit("build complete");

        Ok(BuildResult {
            reference: format!("{}:{}", registered.name, registered.tag),
            content_hash: registered.content_hash,
            disk_path: registered.disk_path,
            image_size: registered.size_bytes,
            cached: false,
            image_id: registered.id,
        })
    }

    /// Dry-run: plan what a build would do without side effects.
    pub fn plan_build(&self, request: BuildRequest) -> Result<BuildPlan, HyphaeError> {
        let is_binary_mode = request.binary_path.is_some();
        let is_docker_mode = request.docker_image.is_some();

        // Validate source path.
        if is_docker_mode {
            // Docker mode: no local path to validate
        } else if is_binary_mode {
            let bp = request.binary_path.as_ref().unwrap();
            if !bp.exists() {
                return Err(OrchestratorError::ProjectDirNotFound {
                    path: bp.clone(),
                }
                .into());
            }
        } else if !request.project_dir.exists() {
            return Err(OrchestratorError::ProjectDirNotFound {
                path: request.project_dir.clone(),
            }
            .into());
        }

        // Derive reference.
        let name = match &request.name {
            Some(n) => n.clone(),
            None => {
                if is_docker_mode {
                    let img = request.docker_image.as_ref().unwrap();
                    let without_tag = img.split(':').next().unwrap_or(img);
                    let short = without_tag.rsplit('/').next().unwrap_or(without_tag);
                    short.to_string()
                } else if is_binary_mode {
                    let bp = request.binary_path.as_ref().unwrap();
                    bp.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "binary".to_string())
                } else {
                    derive_image_name(&request.project_dir)
                        .map_err(|e| OrchestratorError::DryRunFailed(e.to_string()))?
                }
            }
        };
        let tag = &request.tag;
        let reference = if tag == "latest" {
            name.clone()
        } else {
            format!("{name}:{tag}")
        };

        let mut actions = Vec::new();

        if is_docker_mode {
            actions.push(PlannedAction {
                action_type: ActionType::Skip,
                description: format!(
                    "docker mode: convert {}",
                    request.docker_image.as_ref().unwrap()
                ),
            });
        } else if is_binary_mode {
            actions.push(PlannedAction {
                action_type: ActionType::Skip,
                description: format!(
                    "binary mode: package {}",
                    request.binary_path.as_ref().unwrap().display()
                ),
            });
        } else {
            // Detect project type.
            let project_type = detect_project_type(&request.project_dir)?;
            actions.push(PlannedAction {
                action_type: ActionType::Skip,
                description: format!("detect project type: {project_type:?}"),
            });
        }

        // Determine cache status.
        let cache = if request.force {
            CacheStatus::Bypassed
        } else if is_docker_mode {
            let img = request.docker_image.as_ref().unwrap();
            use sha2::{Sha256, Digest};
            let hash = Sha256::digest(img.as_bytes());
            let content_hash = hex::encode(hash);
            match self
                .registry
                .find_by_hash(&content_hash)
                .map_err(|e| OrchestratorError::DryRunFailed(e.to_string()))?
            {
                Some(existing) => CacheStatus::Hit {
                    content_hash,
                    image_id: existing.id,
                },
                None => CacheStatus::Miss { content_hash },
            }
        } else {
            let hash_path = if is_binary_mode {
                let bp = request.binary_path.as_ref().unwrap();
                if bp.is_file() {
                    bp.parent().unwrap_or(bp).to_path_buf()
                } else {
                    bp.clone()
                }
            } else {
                request.project_dir.clone()
            };
            let content_hash = hash_source_directory(&hash_path)
                .map_err(|e| OrchestratorError::DryRunFailed(e.to_string()))?;

            match self
                .registry
                .find_by_hash(&content_hash)
                .map_err(|e| OrchestratorError::DryRunFailed(e.to_string()))?
            {
                Some(existing) => CacheStatus::Hit {
                    content_hash,
                    image_id: existing.id,
                },
                None => CacheStatus::Miss { content_hash },
            }
        };

        // Plan actions based on cache status.
        match &cache {
            CacheStatus::Hit { .. } => {
                actions.push(PlannedAction {
                    action_type: ActionType::Skip,
                    description: "build rootfs (cache hit, skipped)".to_string(),
                });
                actions.push(PlannedAction {
                    action_type: ActionType::Skip,
                    description: "register image (already registered)".to_string(),
                });
            }
            CacheStatus::Miss { .. } | CacheStatus::Bypassed => {
                actions.push(PlannedAction {
                    action_type: ActionType::Create,
                    description: "build rootfs image".to_string(),
                });

                let (_, parsed_tag) = parse_image_ref(&reference);
                let existing = self
                    .registry
                    .find_by_name_tag(&name, &parsed_tag)
                    .map_err(|e| OrchestratorError::DryRunFailed(e.to_string()))?;

                if existing.is_some() {
                    actions.push(PlannedAction {
                        action_type: ActionType::Update,
                        description: format!("re-register image {reference} (replacing existing)"),
                    });
                } else {
                    actions.push(PlannedAction {
                        action_type: ActionType::Create,
                        description: format!("register image {reference}"),
                    });
                }
            }
        }

        Ok(BuildPlan {
            actions,
            cache,
            reference,
        })
    }
}
