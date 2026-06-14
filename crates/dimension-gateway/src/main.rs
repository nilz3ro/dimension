//! Dimension gateway server entry point.
//!
//! Implements the complete server lifecycle:
//!
//! 1. **Tracing** -- structured logging (JSON/compact)
//! 2. **Config** -- CLI/env parsing via clap
//! 3. **Pre-flight** -- validate kernel + bundles exist (real mode only)
//! 4. **Handler** -- MockEchoHandler or VmOrchestrationHandler
//! 5. **Tokens** -- shutdown_token (signal) + drain_token (independent, drain enforcer)
//! 6. **State** -- AppState with startup_time, drain_token, etc.
//! 7. **Router** -- /health (public) + /messages (auth + concurrency)
//! 8. **Reaper** -- background orphan cleanup (real mode only)
//! 9. **Bind** -- TcpListener (readiness signal)
//! 10. **Serve** -- axum with graceful shutdown
//! 11. **Drain** -- timeout enforcer cancels SSE streams
//! 12. **Sweep** -- final orphan recovery (real mode only)
//!
//! **Mock mode** (`--mock`): Skips pre-flight validation, reaper, and
//! final orphan sweep. Uses [`MockEchoHandler`] for development/testing.
//!
//! **Real mode** (default): Full lifecycle with VM orchestration.
//!
//! **CRITICAL:** No `process::exit()` in the shutdown path. The only
//! acceptable uses are pre-flight validation failures (before any VMs
//! exist). After that, `main()` returns naturally so all Rust
//! destructors (especially [`VmLifecycleGuard`] Drop impls) fire.

use std::sync::Arc;
use std::time::{Duration, Instant};
use dimension_store::{ArtifactStore, DeploymentStore, NamedVolumeStore, PgSessionStore, SecretStore, SessionStore, StorageStore, UserStore, VolumeStore};

use clap::Parser;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use dimension_gateway::bundle_store::BundleJobStore;
use dimension_gateway::config::AppConfig;
use dimension_gateway::lifecycle::shutdown::{run_final_orphan_sweep, shutdown_signal};
use dimension_gateway::lifecycle::startup::validate_prerequisites;
use dimension_gateway::worker::registry::WorkerRegistry;
use dimension_gateway::worker::worker_proto::HealthRequest;
// Reaper disabled — see step 9 in main()
// use dimension_gateway::orchestration::reaper::spawn_reaper;
use dimension_gateway::resilience::ConcurrencyController;
use dimension_gateway::server::{build_router, AppState};

use hyphae_core::registry::Registry;

#[tokio::main]
async fn main() {
    // ── 1. Initialize tracing ──────────────────────────────────────────
    let log_broadcaster = dimension_gateway::observability::init_tracing();

    // ── 2. Parse config ────────────────────────────────────────────────
    let config = AppConfig::parse();

    // ── 3. Pre-flight validation (real mode only) ──────────────────────
    // Per CONTEXT.md: "Initialize all subsystems first, then bind the
    // HTTP listener -- if the port is open, the gateway is ready."
    if !config.mock {
        let registry_path = config.resolved_registry_path();
        let registry = Registry::open(&registry_path).unwrap_or_else(|e| {
            tracing::error!(
                path = %registry_path.display(),
                error = %e,
                "failed to open hyphae registry"
            );
            eprintln!(
                "error: failed to open hyphae registry at {}: {e}",
                registry_path.display()
            );
            eprintln!("hint: run 'hyphae build' first, or use --mock for development");
            std::process::exit(1);
        });

        match validate_prerequisites(&registry, &config.kernel_path) {
            Ok(bundle_count) => {
                info!(bundles = bundle_count, "pre-flight validation passed");
            }
            Err(msg) => {
                eprintln!("error: pre-flight validation failed:\n{msg}");
                std::process::exit(1);
            }
        }
        // Registry is dropped here -- it is NOT Send+Sync and cannot be
        // stored in AppState. VmOrchestrationHandler creates its own
        // Registry reference (Phase 5).
    } else {
        info!("mock mode: skipping pre-flight validation");
    }

    // ── 4. Build handler ───────────────────────────────────────────────
    let concurrency_controller = Arc::new(ConcurrencyController::new(config.max_concurrent));
    info!(max_concurrent = config.max_concurrent, "concurrency limit initialized");

    let resource_caps = config.resource_caps();

    // ── 5. Create shutdown token hierarchy ─────────────────────────────
    // shutdown_token: cancelled by signal handler -> triggers axum graceful shutdown
    // drain_token: independent token, cancelled by drain timeout enforcer -> terminates SSE streams
    let shutdown_token = CancellationToken::new();
    let drain_token = CancellationToken::new();

    // ── 5a. Connect to Vault (optional -- platform runs without Vault) ──
    // Vault must be connected before VmOrchestrationHandler so it can be passed in
    // for per-VM token minting and secret injection at launch.
    let vault_client = match dimension_gateway::vault::VaultClient::connect(config.vault.clone()).await {
        Ok(client) => {
            info!("vault client connected");
            Some(Arc::new(client))
        }
        Err(dimension_gateway::vault::VaultError::NotConfigured) => {
            info!("vault not configured (VAULT_ROLE_ID / VAULT_SECRET_ID not set) -- secrets disabled");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "vault connection failed -- starting in degraded mode");
            None
        }
    };

    // ── 5b. Connect to MinIO (optional -- platform runs without MinIO) ──
    let storage_client = match dimension_gateway::storage::MinioClient::connect(config.storage.clone()) {
        Ok(client) => {
            info!("minio storage client connected");
            Some(Arc::new(client))
        }
        Err(dimension_gateway::storage::StorageError::NotConfigured) => {
            info!("minio not configured (MINIO_ACCESS_KEY / MINIO_SECRET_KEY not set) -- storage disabled");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "minio connection failed -- storage disabled");
            None
        }
    };

    // ── 6. Build AppState with handler ─────────────────────────────────

    // ── 6a-pre. Worker registry (multi-host mode) ──────────────────────
    let worker_registry: Option<Arc<WorkerRegistry>> = if config.multi_host {
        info!("multi-host mode enabled: creating worker registry");
        Some(Arc::new(WorkerRegistry::new()))
    } else {
        info!("single-host mode: worker registry disabled");
        None
    };

    // ── 6a. Connect to database and ensure bootstrap admin ──────────────
    let store = PgSessionStore::connect(&config.database_url)
        .await
        .expect("failed to connect to database");
    let store = Arc::new(store);

    // Ensure bootstrap admin exists (idempotent -- only creates on first run)
    match store.ensure_bootstrap_admin().await {
        Ok(Some(key)) => {
            info!(key_prefix = &key[..15], "bootstrap admin created -- save this API key (shown once)");
            // Print to stderr so it's visible even without structured logging
            eprintln!("BOOTSTRAP ADMIN API KEY: {}", key);
        }
        Ok(None) => {
            tracing::debug!("bootstrap admin already exists");
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to ensure bootstrap admin");
            std::process::exit(1);
        }
    }

    // ── 6b. Create session store, secret store, storage store, task store, volume store, and artifact store ──
    let session_store: Arc<dyn SessionStore> = store.clone() as Arc<dyn SessionStore>;
    let secret_store: Arc<dyn SecretStore> = store.clone() as Arc<dyn SecretStore>;
    let storage_store: Arc<dyn StorageStore> = store.clone() as Arc<dyn StorageStore>;
    let task_store: Arc<dyn dimension_store::TaskStore> = store.clone() as Arc<dyn dimension_store::TaskStore>;
    let volume_store: Arc<dyn VolumeStore> = store.clone() as Arc<dyn VolumeStore>;
    let artifact_store: Arc<dyn ArtifactStore> = store.clone() as Arc<dyn ArtifactStore>;
    let deployment_store: Arc<dyn DeploymentStore> = store.clone() as Arc<dyn DeploymentStore>;
    let named_volume_store: Arc<dyn NamedVolumeStore> = store.clone() as Arc<dyn NamedVolumeStore>;

    // Resolve the registry path once (used by AppState).
    let registry_path = config.resolved_registry_path();

    // ── 6b-post. Create shared HTTP client ──────────────────────────────
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client");

    // ── 6c. Create BundleJobStore ───────────────────────────────────────
    let bundle_job_store = BundleJobStore::new();

    let state = AppState {
        config: config.clone(),
        concurrency_controller,
        resource_caps,
        startup_time: Instant::now(),
        drain_token: drain_token.clone(),
        user_store: store.clone() as Arc<dyn dimension_store::UserStore>,
        session_store,
        bundle_job_store: bundle_job_store.clone(),
        registry_path,
        vault_client,
        secret_store,
        storage_client,
        storage_store,
        task_store,
        worker_registry: worker_registry.clone(),
        volume_store,
        artifact_store,
        deployment_store,
        named_volume_store,
        log_broadcaster,
        http_client: http_client.clone(),
        clickhouse_client: {
            let ch = clickhouse::Client::default()
                .with_url(&config.clickhouse_url)
                .with_database(&config.clickhouse_database);
            // Clickhouse client is always constructed — queries will fail at
            // runtime if the server is unreachable (same degraded-mode pattern
            // as other Optional clients, but CH has no connect-time check).
            Some(ch)
        },
        log_storage_client: {
            // Build opendal S3 operator for invocation log retrieval.
            // Returns None when access keys are not configured.
            match (&config.log_minio_access_key, &config.log_minio_secret_key) {
                (Some(ak), Some(sk)) => {
                    let builder = opendal::services::S3::default()
                        .endpoint(&config.log_minio_endpoint)
                        .region("auto")
                        .bucket(&config.log_minio_bucket)
                        .access_key_id(ak)
                        .secret_access_key(sk)
                        .root("/");
                    match opendal::Operator::new(builder) {
                        Ok(op) => {
                            info!("log MinIO client connected for invocation log retrieval");
                            Some(op.finish())
                        }
                        Err(e) => {
                            warn!(error = %e, "failed to build log MinIO operator, log retrieval disabled");
                            None
                        }
                    }
                }
                _ => {
                    info!("log MinIO not configured, invocation log retrieval disabled");
                    None
                }
            }
        },
        pulsar_client: {
            if let Some(url) = config.pulsar_url.as_deref() {
                match dimension_gateway::pulsar::PulsarClient::connect(url, &config.pulsar_topic).await {
                    Ok(p) => {
                        info!(url = %url, topic = %config.pulsar_topic, "Pulsar consumer connected");
                        Some(p)
                    }
                    Err(e) => {
                        warn!(error = %e, url = %url, "Pulsar connect failed — live event tail disabled");
                        None
                    }
                }
            } else {
                info!("PULSAR_URL not set — run-events live tail will be unavailable");
                None
            }
        },
    };


    // ── 6f-post. Startup reconciliation (multi-host mode only) ────────────
    // Must run after deployment_store and worker_registry are ready,
    // but BEFORE the HTTP listener is bound so no traffic is served yet.
    if config.multi_host
        && let Some(ref registry) = worker_registry
    {
            reconcile_deployments_on_startup(
                state.deployment_store.clone(),
                registry.clone(),
            ).await;
    }

    // ── 6f-post2. Deployment health probe background task ─────────────────
    let probe_store = state.deployment_store.clone();
    let probe_cancel = shutdown_token.child_token();
    tokio::spawn(async move {
        run_deployment_health_probes(probe_store, probe_cancel).await;
    });
    info!(
        interval_secs = DEPLOYMENT_PROBE_INTERVAL_SECS,
        max_failures = DEPLOYMENT_PROBE_MAX_FAILURES,
        "deployment health probe task started"
    );

    // ── 6g. Spawn worker health polling (multi-host mode only) ────────────
    if let Some(ref registry) = worker_registry {
        let health_registry = registry.clone();
        let health_interval = config.worker_health_interval_secs;
        let health_cancel = shutdown_token.child_token();

        tokio::spawn(async move {
            run_worker_health_polling(health_registry, health_interval, health_cancel).await;
        });
        info!(
            interval_secs = config.worker_health_interval_secs,
            "worker health polling started"
        );
    }

    // ── 6h. Artifact TTL GC task (temporarily disabled) ──────────────────
    // NOTE: The artifact GC was part of crate::tasks::gc which was removed
    // with the legacy task pipeline (T01). It should be restored as a
    // standalone module in a follow-up task.
    // dimension_gateway::tasks::gc::spawn_artifact_gc(
    //     state.artifact_store.clone(),
    //     state.storage_client.clone(),
    //     drain_token.clone(),
    // );
    // info!(interval_secs = 3600, "artifact GC task started (1-hour interval)");
    info!("artifact GC task disabled (pending module restoration)");

    // ── 7. Build router ────────────────────────────────────────────────
    let app = build_router(state);

    // ── 7a. Spawn bundle job reaper (5-minute interval, 1-hour TTL) ────
    let job_reaper_cancel = CancellationToken::new();
    let job_reaper_store = bundle_job_store.clone();
    let job_reaper_token = job_reaper_cancel.clone();
    let job_reaper_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300)); // 5 minutes
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    job_reaper_store.reap_stale(3600); // 1 hour
                    tracing::trace!("reaped stale bundle conversion jobs");
                }
                _ = job_reaper_token.cancelled() => {
                    tracing::debug!("bundle job reaper shutting down");
                    break;
                }
            }
        }
    });
    info!("bundle job reaper started (5-minute interval, 1-hour TTL)");

    // ── 9. Orphan reaper DISABLED ─────────────────────────────────────
    // The orphan reaper has a race condition: it can't distinguish active
    // VMs from orphans, causing it to kill VMs mid-request. Disabled until
    // an active VM registry is implemented (see PLANNING.md).
    let reaper_cancel = CancellationToken::new();
    let reaper_handle: Option<JoinHandle<()>> = None;
    info!("orphan reaper disabled (pending active VM registry fix)");

    // ── 9. Bind listener (readiness signal) ────────────────────────────
    let bind_addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("failed to bind TCP listener");

    info!(address = %bind_addr, mock = config.mock, "gateway ready");

    // ── 10. Spawn drain timeout enforcer ───────────────────────────────
    // Waits for shutdown signal, then gives in-flight requests
    // drain_timeout_secs to complete. If they don't, cancels drain_token
    // which force-terminates all SSE streams.
    let drain_timeout = Duration::from_secs(config.drain_timeout_secs);
    let drain_token_for_timer = drain_token.clone();
    let shutdown_token_for_timer = shutdown_token.clone();
    tokio::spawn(async move {
        shutdown_token_for_timer.cancelled().await;
        tracing::info!(
            drain_timeout_secs = drain_timeout.as_secs(),
            "draining in-flight requests"
        );
        tokio::time::sleep(drain_timeout).await;
        tracing::warn!("drain timeout expired, forcing stream closure");
        drain_token_for_timer.cancel();
    });

    // ── 11. Run server with graceful shutdown ──────────────────────────
    // When shutdown_signal fires: shutdown_token is cancelled, axum stops
    // accepting new TCP connections. Existing connections continue until
    // their response bodies complete (or drain_token cancels them).
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_token.clone()))
        .await
        .expect("server error");

    // ── 12. Server stopped ─────────────────────────────────────────────
    info!("all connections closed");

    // Cancel and await bundle job reaper.
    job_reaper_cancel.cancel();
    let _ = job_reaper_handle.await;
    info!("bundle job reaper stopped");

    // Cancel reaper and wait for it to finish.
    reaper_cancel.cancel();
    if let Some(handle) = reaper_handle {
        let _ = handle.await;
    }

    // ── 13. Final orphan sweep (real mode only) ────────────────────────
    // Safety net: after all tasks have been cancelled and guards have
    // dropped, sweep for any remaining Firecracker processes.
    // Do NOT call process::exit() -- Drop must fire for VmLifecycleGuards.
    if !config.mock {
        run_final_orphan_sweep().await;
    }

    info!("gateway shutdown complete");
}

/// Background task: poll all registered workers for health and remove stale ones.
///
/// On each interval:
/// 1. For each registered worker, call Health RPC.
/// 2. On success: update registry with fresh resource data and running VM count.
/// 3. On failure: increment miss counter. After 3 consecutive misses, remove worker.
async fn run_worker_health_polling(
    registry: Arc<WorkerRegistry>,
    interval_secs: u64,
    cancel: tokio_util::sync::CancellationToken,
) {
    use std::collections::HashMap;
    use std::time::Duration;

    let mut miss_counts: HashMap<uuid::Uuid, u32> = HashMap::new();
    let interval = Duration::from_secs(interval_secs);

    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = cancel.cancelled() => {
                tracing::debug!("worker health polling shutting down");
                return;
            }
        }

        let workers = registry.get_all();
        for worker in workers {
            let worker_id = worker.worker_id;
            let mut client = worker.client.clone();

            match client.health(HealthRequest {}).await {
                Ok(resp) => {
                    let health = resp.into_inner();
                    let available = health.available.unwrap_or_default();
                    registry.update_health(
                        worker_id,
                        available.memory_mb,
                        available.vcpus,
                        health.running_vms,
                        health.draining,
                    );
                    miss_counts.remove(&worker_id);
                    tracing::trace!(
                        worker_id = %worker_id,
                        running_vms = health.running_vms,
                        draining = health.draining,
                        "worker health ok"
                    );
                }
                Err(e) => {
                    let misses = miss_counts.entry(worker_id).or_insert(0);
                    *misses += 1;
                    tracing::warn!(
                        worker_id = %worker_id,
                        misses = *misses,
                        error = %e,
                        "worker health poll failed"
                    );
                    if *misses >= 3 {
                        tracing::warn!(
                            worker_id = %worker_id,
                            "removing worker after 3 consecutive health poll failures"
                        );
                        registry.remove(worker_id);
                        miss_counts.remove(&worker_id);
                    }
                }
            }
        }
    }
}

// ── Deployment health probe constants ─────────────────────────────────────────

const DEPLOYMENT_PROBE_INTERVAL_SECS: u64 = 30;
const DEPLOYMENT_PROBE_MAX_FAILURES: i32 = 3;

/// Background task: send HTTP health probes to all active deployment VMs.
///
/// On each interval:
/// 1. Query all deployments in 'health_checking' or 'healthy' status with a guest_ip.
/// 2. For each, send GET http://{guest_ip}:{probe_port}/health with a 5s timeout.
/// 3. On 2xx success: reset probe_failures, ensure status = 'healthy'.
/// 4. On failure: increment probe_failures. After >= 3 failures: status = 'unhealthy'.
async fn run_deployment_health_probes(
    store: Arc<dyn dimension_store::DeploymentStore>,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(
        std::time::Duration::from_secs(DEPLOYMENT_PROBE_INTERVAL_SECS),
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("failed to build probe HTTP client");

    loop {
        tokio::select! {
            _ = interval.tick() => {
                run_probe_round(&client, store.clone()).await;
            }
            _ = cancel.cancelled() => {
                tracing::debug!("deployment health probe task shutting down");
                break;
            }
        }
    }
}

async fn run_probe_round(
    client: &reqwest::Client,
    store: Arc<dyn dimension_store::DeploymentStore>,
) {
    let deployments = match store.list_probeable_deployments().await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "failed to list deployments for health probe");
            return;
        }
    };

    for deployment in deployments {
        let Some(ref guest_ip) = deployment.guest_ip else { continue };
        let url = format!("http://{}:{}/health", guest_ip, deployment.probe_port);
        let deployment_id = deployment.id;

        let probe_result = client.get(&url).send().await;

        match probe_result {
            Ok(resp) if resp.status().is_success() => {
                // Healthy: reset failures and ensure status is 'healthy'
                let _ = store.reset_probe_failures(deployment_id).await;
                if deployment.status != "healthy" {
                    let _ = store.update_deployment_status(deployment_id, "healthy").await;
                    tracing::info!(
                        deployment_id = %deployment_id,
                        name = %deployment.name,
                        "deployment became healthy"
                    );
                }
            }
            other => {
                let fail_reason = match other {
                    Ok(resp) => format!("HTTP {}", resp.status()),
                    Err(e) => e.to_string(),
                };
                let failures = match store.increment_probe_failures(deployment_id).await {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                tracing::debug!(
                    deployment_id = %deployment_id,
                    failures,
                    reason = %fail_reason,
                    "deployment probe failed"
                );
                if failures >= DEPLOYMENT_PROBE_MAX_FAILURES {
                    let _ = store.update_deployment_status(deployment_id, "unhealthy").await;
                    tracing::warn!(
                        deployment_id = %deployment_id,
                        name = %deployment.name,
                        failures,
                        "deployment marked unhealthy after probe failures"
                    );
                }
            }
        }
    }
}

/// On gateway startup, mark deployments as orphaned if their worker is no longer registered.
///
/// Algorithm:
/// 1. List all distinct worker_ids with active (non-stopped) deployments.
/// 2. For each worker_id NOT in the live registry, mark its deployments orphaned.
///
/// Workers re-register via HTTP heartbeat on restart. By the time this runs
/// (before serving traffic), the registry has all live workers.
async fn reconcile_deployments_on_startup(
    deployment_store: Arc<dyn dimension_store::DeploymentStore>,
    registry: Arc<WorkerRegistry>,
) {
    use std::collections::HashSet;

    // Collect live worker IDs from the in-memory registry
    let live_worker_ids: HashSet<String> = registry
        .get_all()
        .into_iter()
        .map(|w| w.worker_id.to_string())
        .collect();

    // Find all worker_ids with active deployments
    match deployment_store.list_active_worker_ids().await {
        Ok(worker_ids) => {
            for worker_id in worker_ids {
                if !live_worker_ids.contains(&worker_id) {
                    tracing::warn!(
                        %worker_id,
                        "worker not in registry on startup — marking its deployments orphaned"
                    );
                    match deployment_store.mark_orphaned_for_worker(&worker_id).await {
                        Ok(count) => tracing::info!(
                            %worker_id,
                            count,
                            "marked deployments orphaned for missing worker"
                        ),
                        Err(e) => tracing::warn!(
                            %worker_id,
                            error = %e,
                            "failed to mark deployments orphaned"
                        ),
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "deployment startup reconciliation skipped (store query failed)"
            );
        }
    }
    tracing::info!("deployment startup reconciliation complete");
}
