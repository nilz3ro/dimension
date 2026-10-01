//! Dimension worker binary entry point.
//!
//! The worker runs:
//! 1. A gRPC server (WorkerService) that receives Execute RPCs from the gateway.
//! 2. A registration-and-heartbeat loop that registers with the gateway via HTTP POST,
//!    retrying with exponential backoff until success, then heartbeating every N seconds.
//! 3. A graceful shutdown path: on SIGTERM/SIGINT, set draining=true and wait
//!    for in-flight VMs to finish before exiting.

use std::sync::Arc;
use std::time::Duration;

use backon::{ExponentialBuilder, Retryable};
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tracing::{debug, info, warn};

use dimension_gateway::orchestration::config::{CidAllocator, OrchestrationConfig};
use dimension_gateway::orchestration::deployment::DeploymentVmManager;
use hyphae_core::net::LanAllow;

mod config;
mod events;
mod observability;
mod pulsar;
mod service;

// Include the generated gRPC code.
pub mod proto {
    tonic::include_proto!("dimension.worker");
}

use config::WorkerConfig;
use proto::worker_service_server::WorkerServiceServer;
use service::WorkerServiceImpl;

#[tokio::main]
async fn main() {
    // ── 1. Initialize tracing ──────────────────────────────────────────
    dimension_gateway::observability::init_tracing();

    // ── 2. Parse config ────────────────────────────────────────────────
    let config = WorkerConfig::parse();

    info!(
        grpc_addr = %config.grpc_addr,
        advertise_addr = %config.effective_advertise_addr(),
        memory_mb = config.memory_mb,
        vcpus = config.vcpus,
        gateway_url = %config.gateway_url,
        heartbeat_interval_secs = config.heartbeat_interval_secs,
        "dimension-worker starting"
    );

    // ── 3. Setup registry, config, and allocators ──────────────────────
    let jailer_bin = if config.require_jail {
        let jailer_bin = match config.jailer_bin.clone() {
            Some(path) => path,
            None => hyphae_core::jail::find_jailer(Some(&config.firecracker_bin)).unwrap_or_else(
                |e| {
                    tracing::error!(error = %e, "jailer is required but could not be discovered");
                    eprintln!("error: jailer is required but could not be discovered: {e}");
                    eprintln!("hint: set DIMENSION_JAILER_BIN to the jailer binary path");
                    std::process::exit(1);
                },
            ),
        };

        if !jailer_bin.is_file() {
            tracing::error!(path = %jailer_bin.display(), "configured jailer binary is not a file");
            eprintln!(
                "error: configured jailer binary is not a file: {}",
                jailer_bin.display()
            );
            eprintln!("hint: set DIMENSION_JAILER_BIN to the jailer binary path");
            std::process::exit(1);
        }

        let jail_user = hyphae_core::jail::validate_jail_user().unwrap_or_else(|e| {
            tracing::error!(error = %e, "jailer is required but the jail user is invalid");
            eprintln!("error: jailer is required but the jail user is invalid: {e}");
            std::process::exit(1);
        });
        info!(
            path = %jailer_bin.display(),
            uid = jail_user.uid,
            gid = jail_user.gid,
            "required jailer configuration validated"
        );
        Some(jailer_bin)
    } else {
        config.jailer_bin.clone()
    };

    let registry_path = config.resolved_registry_path();
    // Verify the registry is accessible at startup.
    let _registry = hyphae_core::registry::Registry::open(&registry_path).unwrap_or_else(|e| {
        tracing::error!(path = %registry_path.display(), error = %e, "failed to open hyphae registry");
        eprintln!("error: failed to open hyphae registry at {}: {e}", registry_path.display());
        eprintln!("hint: run 'hyphae build' first");
        std::process::exit(1);
    });

    let cid_allocator = Arc::new(CidAllocator::new());
    // Parse the LAN allowlist up front so an invalid entry fails startup
    // (fail closed) rather than silently widening or narrowing egress.
    let lan_allow: Vec<LanAllow> = config
        .lan_allow
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(LanAllow::parse)
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| {
            eprintln!("invalid DIMENSION_LAN_ALLOW entry: {e}");
            std::process::exit(1);
        });
    if !lan_allow.is_empty() {
        info!(
            entries = lan_allow
                .iter()
                .map(|e| format!("{}:{}", e.cidr, e.port))
                .collect::<Vec<_>>()
                .join(","),
            "worker: LAN egress allowlist active"
        );
    }

    let orch_config = OrchestrationConfig {
        boot_timeout: Duration::from_secs(config.boot_timeout_secs),
        processing_timeout: Duration::from_secs(config.processing_timeout_secs),
        max_boot_timeout: Duration::from_secs(config.max_boot_timeout_secs),
        max_processing_timeout: Duration::from_secs(config.max_processing_timeout_secs),
        kernel_path: config.kernel_path.clone(),
        firecracker_bin: config.firecracker_bin.clone(),
        mock: false,
        enable_network: config.enable_network,
        lan_allow,
        suppress_guest_stderr: true,
        jailer_bin,
        require_jail: config.require_jail,
        chroot_base_dir: config.chroot_base_dir.clone(),
    };

    // ── 4. Create DeploymentVmManager (worker mode — no DeploymentStore) ──
    let deployment_manager = Arc::new(DeploymentVmManager::new_for_worker(
        orch_config.clone(),
        cid_allocator.clone(),
        registry_path.clone(),
    ));

    // ── 6. Create observability clients (optional — graceful degradation) ──
    let clickhouse_client = {
        let ch = observability::ClickhouseClient::new(
            &config.clickhouse_url,
            &config.clickhouse_database,
        );
        let invocations_ok = ch.ensure_table().await;
        let events_ok = ch.ensure_run_events_table().await;
        match (invocations_ok, events_ok) {
            (Ok(()), Ok(())) => {
                info!(
                    url = %config.clickhouse_url,
                    database = %config.clickhouse_database,
                    "Clickhouse invocations + run_events tables ensured"
                );
                Some(ch)
            }
            (inv, ev) => {
                warn!(
                    invocations_error = ?inv.err(),
                    run_events_error = ?ev.err(),
                    url = %config.clickhouse_url,
                    "Clickhouse not available — telemetry disabled"
                );
                None
            }
        }
    };

    // ── Pulsar producer (optional) ─────────────────────────────────────
    let pulsar_publisher = if let Some(url) = config.pulsar_url.as_deref() {
        match pulsar::PulsarPublisher::connect(url, &config.pulsar_topic).await {
            Ok(p) => {
                info!(
                    url = %url,
                    topic = %config.pulsar_topic,
                    "Pulsar producer connected"
                );
                Some(p)
            }
            Err(e) => {
                warn!(error = %e, url = %url, "Pulsar connect failed — publish disabled");
                None
            }
        }
    } else {
        info!("PULSAR_URL not set — run events will go to Clickhouse and in-process broadcast only");
        None
    };

    let event_fanout = events::EventFanout::spawn(clickhouse_client.clone(), pulsar_publisher);

    let log_uploader = observability::LogUploader::new(
        config.log_minio_endpoint.as_deref().unwrap_or("http://localhost:9000"),
        &config.log_minio_bucket,
        config.log_minio_access_key.as_deref(),
        config.log_minio_secret_key.as_deref(),
    );
    if log_uploader.is_some() {
        info!(
            bucket = %config.log_minio_bucket,
            "MinIO log uploader configured"
        );
    } else {
        warn!("MinIO log upload credentials not set — invocation log uploads disabled");
    }

    // ── Orphan network reconciliation ──────────────────────────────────
    // A crashed worker leaks TAP devices and their iptables rules (the
    // terminal-path cleanup never ran). Recover them BEFORE constructing
    // the SubnetAllocator — its constructor scans /sys/class/net and would
    // otherwise permanently reserve the leaked indices.
    if orch_config.enable_network {
        let report = hyphae_core::net::recover_orphan_network(&orch_config.lan_allow);
        if report.taps_found > 0 {
            info!(
                taps_found = report.taps_found,
                taps_deleted = report.taps_deleted,
                nat_rule_sets_removed = report.nat_rule_sets_removed,
                "worker: recovered orphaned VM network resources"
            );
        }
    }

    // ── 7. Create WorkerServiceImpl ─────────────────────────────────────
    let worker_id = uuid::Uuid::new_v4().to_string();
    let service = Arc::new(WorkerServiceImpl::new(
        deployment_manager,
        worker_id.clone(),
        config.memory_mb,
        config.vcpus,
        registry_path,
        orch_config,
        cid_allocator,
        Arc::new(tokio::sync::Mutex::new(hyphae_core::net::SubnetAllocator::new())),
        clickhouse_client,
        log_uploader,
        config.log_minio_bucket.clone(),
        event_fanout.sink.clone(),
        event_fanout.broadcast.clone(),
    ));

    let draining = service.draining.clone();
    let running_vms = service.running_vms.clone();

    // ── 8. Start gRPC server ───────────────────────────────────────────
    let grpc_addr: std::net::SocketAddr = config.grpc_addr.parse().unwrap_or_else(|e| {
        eprintln!("error: invalid grpc_addr {}: {e}", config.grpc_addr);
        std::process::exit(1);
    });

    info!(addr = %grpc_addr, "gRPC server starting");

    let grpc_service = WorkerServiceServer::from_arc(service)
        .max_decoding_message_size(1024 * 1024 * 1024)   // 1 GiB
        .max_encoding_message_size(1024 * 1024 * 1024);   // 1 GiB

    // ── 9. CancellationToken for coordinated shutdown ──────────────────
    let shutdown_token = CancellationToken::new();

    // ── 10. Register and heartbeat with gateway (background task) ──────
    let gateway_url = config.gateway_url.clone();
    let advertise_addr = config.effective_advertise_addr().to_string();
    let worker_id_for_reg = worker_id.clone();
    let memory_mb = config.memory_mb;
    let vcpus = config.vcpus;
    let heartbeat_interval_secs = config.heartbeat_interval_secs;
    let shutdown_for_reg = shutdown_token.child_token();

    tokio::spawn(async move {
        registration_and_heartbeat_loop(
            gateway_url,
            worker_id_for_reg,
            advertise_addr,
            memory_mb,
            vcpus,
            heartbeat_interval_secs,
            shutdown_for_reg,
        )
        .await;
    });

    // ── 11. Graceful shutdown on SIGTERM/SIGINT ────────────────────────
    let draining_for_shutdown = draining.clone();
    let running_vms_for_shutdown = running_vms.clone();
    let worker_id_for_shutdown = worker_id.clone();
    tokio::spawn(async move {
        wait_for_signal().await;
        info!(worker_id = %worker_id_for_shutdown, "shutdown signal received, initiating drain");
        draining_for_shutdown.store(true, std::sync::atomic::Ordering::Relaxed);

        // Cancel the registration/heartbeat loop immediately on shutdown.
        shutdown_token.cancel();

        // Wait up to 60 seconds for in-flight VMs to finish.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let count = running_vms_for_shutdown.load(std::sync::atomic::Ordering::Relaxed);
            if count <= 0 {
                info!("all VMs finished, shutting down");
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                warn!(running = count, "drain timeout expired, forcing shutdown");
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        std::process::exit(0);
    });

    // Run gRPC server (blocking until shutdown).
    Server::builder()
        .add_service(grpc_service)
        .serve(grpc_addr)
        .await
        .expect("gRPC server error");
}

/// Register with the gateway and then heartbeat continuously until shutdown.
///
/// Phase 1: Retry the initial registration POST with exponential backoff (1s-30s, with jitter)
/// until success or cancellation.
///
/// Phase 2: Send a heartbeat (same registration POST) every `heartbeat_interval_secs`.
/// On POST failure, log a warning but keep the loop running — this is the gateway restart
/// survival path. Stop only when `shutdown` is cancelled.
async fn registration_and_heartbeat_loop(
    gateway_url: String,
    worker_id: String,
    grpc_addr: String,
    memory_mb: u64,
    vcpus: u32,
    heartbeat_interval_secs: u64,
    shutdown: CancellationToken,
) {
    let url = format!("{}/internal/workers/register", gateway_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "worker_id": worker_id,
        "grpc_addr": grpc_addr,
        "capacity": {
            "memory_mb": memory_mb,
            "vcpus": vcpus,
        }
    });

    let client = reqwest::Client::new();

    // ── Phase 1: Initial registration with backon infinite retry ───────
    let backoff = ExponentialBuilder::default()
        .with_min_delay(Duration::from_secs(1))
        .with_max_delay(Duration::from_secs(30))
        .with_jitter()
        .without_max_times();

    let register = || {
        let client = client.clone();
        let url = url.clone();
        let body = body.clone();
        async move {
            client
                .post(&url)
                .json(&body)
                .send()
                .await?
                .error_for_status()
        }
    };

    let registration_future = register
        .retry(backoff)
        .notify(|err: &reqwest::Error, dur: Duration| {
            warn!(
                error = %err,
                retry_in = ?dur,
                "gateway registration failed, retrying"
            );
        });

    tokio::select! {
        result = registration_future => {
            match result {
                Ok(_) => {
                    info!("registered with gateway, starting heartbeat loop");
                }
                Err(e) => {
                    // Only happens if backoff exhausts — impossible with without_max_times,
                    // but handle defensively.
                    warn!(error = %e, "registration exhausted retries unexpectedly");
                    return;
                }
            }
        }
        _ = shutdown.cancelled() => {
            info!("shutdown requested during initial registration, stopping");
            return;
        }
    }

    // ── Phase 2: Heartbeat loop ────────────────────────────────────────
    let mut interval = tokio::time::interval(Duration::from_secs(heartbeat_interval_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let result = client
                    .post(&url)
                    .json(&body)
                    .send()
                    .await;

                match result {
                    Ok(resp) if resp.status().is_success() => {
                        debug!("heartbeat sent");
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        warn!(%status, "heartbeat rejected by gateway");
                        // Do NOT break — keep looping.
                    }
                    Err(e) => {
                        warn!(error = %e, "heartbeat failed, gateway may be restarting");
                        // Do NOT break — keep looping.
                    }
                }
            }
            _ = shutdown.cancelled() => {
                info!("heartbeat loop shutting down");
                break;
            }
        }
    }
}

/// Wait for SIGTERM or SIGINT.
async fn wait_for_signal() {
    use tokio::signal;

    #[cfg(unix)]
    {
        use signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM");
        let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT");
        tokio::select! {
            _ = sigterm.recv() => { info!("received SIGTERM"); }
            _ = sigint.recv() => { info!("received SIGINT"); }
        }
    }

    #[cfg(not(unix))]
    {
        signal::ctrl_c().await.expect("failed to register Ctrl+C handler");
        info!("received Ctrl+C");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use backon::{BackoffBuilder, ExponentialBuilder};
    use tokio_util::sync::CancellationToken;

    use super::registration_and_heartbeat_loop;

    /// Test 1: Verify the ExponentialBuilder config produces delays within
    /// the expected 1s-30s range (with jitter).
    #[test]
    fn test_backoff_config_produces_valid_delays() {
        let mut backoff = ExponentialBuilder::default()
            .with_min_delay(Duration::from_secs(1))
            .with_max_delay(Duration::from_secs(30))
            .with_jitter()
            .without_max_times()
            .build();

        // With jitter, each delay is in the range [base, 2*base) capped at max_delay.
        // First delay: base = 1s, with jitter in [1s, 2s).
        let d1 = backoff.next().expect("must produce a delay");
        assert!(
            d1 >= Duration::from_secs(1) && d1 < Duration::from_secs(2),
            "first delay should be in [1s, 2s), got {d1:?}"
        );

        // Second delay: base = 2s, with jitter in [2s, 4s).
        let d2 = backoff.next().expect("must produce a delay");
        assert!(
            d2 >= Duration::from_secs(2) && d2 < Duration::from_secs(4),
            "second delay should be in [2s, 4s), got {d2:?}"
        );

        // Collect delays until we see max_delay (30s) being hit.
        // After several doublings we should reach/saturate at 30s (+ jitter ≤ 60s).
        let mut saw_capped = false;
        for _ in 0..10 {
            if let Some(d) = backoff.next() {
                assert!(
                    d <= Duration::from_secs(60),
                    "delay should never exceed 60s (2*max_delay), got {d:?}"
                );
                if d >= Duration::from_secs(30) {
                    saw_capped = true;
                }
            }
        }
        assert!(saw_capped, "expected some delays at or above max_delay of 30s");

        // Verify it produces unlimited delays (doesn't return None).
        // We've already consumed 12 values from a backoff with without_max_times —
        // the iterator should still be live.
        assert!(
            backoff.next().is_some(),
            "backoff with without_max_times should never exhaust"
        );
    }

    /// Test 2: registration_and_heartbeat_loop stops when cancellation token
    /// is cancelled before the (unreachable) gateway responds.
    #[tokio::test]
    async fn test_cancellation_stops_registration_loop() {
        let shutdown = CancellationToken::new();
        let shutdown_clone = shutdown.clone();

        // Cancel the token after a short delay.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            shutdown_clone.cancel();
        });

        // Point at a non-existent endpoint so registration never succeeds.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            registration_and_heartbeat_loop(
                "http://127.0.0.1:19999".to_string(), // nothing listening here
                "00000000-0000-0000-0000-000000000001".to_string(),
                "127.0.0.1:50099".to_string(),
                512,
                1,
                15,
                shutdown,
            ),
        )
        .await;

        assert!(
            result.is_ok(),
            "registration_and_heartbeat_loop should return (not hang) after cancellation"
        );
    }
}
