//! gRPC WorkerService implementation.
//!
//! Receives Execute RPCs from the gateway, runs VMs via the VmOrchestrationHandler,
//! and streams BackendEvents back to the gateway over the gRPC response stream.
//!
//! Also handles deployment lifecycle RPCs (StartDeployment, StopDeployment,
//! GetDeploymentStatus) by delegating to DeploymentVmManager.
//!
//! The RunInvocation/StopInvocation RPCs support two invocation modes:
//! - **sync**: launch VM, capture stdout over vsock, return inline.
//! - **async**: launch VM, return immediately, VM runs until done.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicI32, Ordering},
    Arc,
};

use async_trait::async_trait;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use dimension_gateway::orchestration::config::{CidAllocator, OrchestrationConfig};
use hyphae_core::net::SubnetAllocator;
use dimension_gateway::orchestration::deployment::DeploymentVmManager;

use crate::events::{EventSink, RunBroadcast, RunEvent, StateEmitter};
use crate::observability::{ClickhouseClient, InvocationRecord, InvocationStatus, LogUploader, now_epoch_ms};
use crate::proto::worker_service_server::WorkerService;
use crate::proto::{
    DrainRequest, DrainResponse, HealthRequest,
    HealthResponse, WorkerResources,
    StartDeploymentRequest, StartDeploymentResponse,
    StopDeploymentRequest, StopDeploymentResponse,
    GetDeploymentStatusRequest, GetDeploymentStatusResponse,
    PushBundleRequest, PushBundleResponse,
    RunInvocationRequest, RunInvocationResponse,
    StopInvocationRequest, StopInvocationResponse,
    SubscribeRunEventsRequest, RunEventProto,
};

/// Tracks the state of an active invocation (sync or async).
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields read by future status RPCs and test assertions
pub struct InvocationState {
    /// Unique invocation identifier.
    pub invocation_id: Uuid,
    /// Bundle that was launched.
    pub bundle_id: String,
    /// Invocation mode: "sync" or "async".
    pub mode: String,
    /// Firecracker process PID.
    pub pid: u32,
    /// Runtime directory for this invocation's VM.
    pub runtime_dir: PathBuf,
    /// VM identifier used for cgroup cleanup.
    pub vm_id: String,
    /// Jail root directory (only set for jailed launches).
    pub jail_root: Option<PathBuf>,
    /// Guest CID allocated for vsock.
    pub guest_cid: u32,
    /// User who dispatched the invocation.
    pub user_id: String,
    /// Wall-clock start time for duration computation.
    pub started_at: std::time::Instant,
    /// Epoch-millis timestamp for Clickhouse created_at.
    pub created_at_ms: i64,
    /// Network resources for this invocation (`None` when networking is
    /// disabled). Retained so `stop_invocation` can release them; sync/async
    /// completion release through their own handle. Release is idempotent,
    /// so racing terminal paths cannot double-clean.
    pub network: Option<Arc<tokio::sync::Mutex<NetworkScope>>>,
}

fn resolve_vsock_host_path(reported_path: Option<&Path>, fallback_path: &Path) -> PathBuf {
    reported_path.unwrap_or(fallback_path).to_path_buf()
}

fn invocation_outcome(
    done: Option<&dimension_protocol::proto::Done>,
    _process_exit_code: i32,
) -> (InvocationStatus, i32) {
    match done {
        Some(done) if done.success => (InvocationStatus::Completed, done.exit_code),
        Some(done) => (InvocationStatus::Failed, done.exit_code),
        None => (InvocationStatus::Failed, -1),
    }
}

/// Physical release of one VM's network resources.
///
/// Trait seam so the invocation lifecycle is unit-testable without root,
/// iptables, or TAP devices: tests observe releases through a recorder
/// while production drops the real RAII resources.
pub(crate) trait NetworkRelease: Send {
    /// Remove the per-VM NAT rules (first) and delete the TAP device.
    /// Ordering matters: the rules reference the TAP interface.
    fn release(self: Box<Self>);
    /// TAP device name, for logging.
    fn tap_name(&self) -> &str;
    /// Subnet allocation index to return to the allocator.
    fn subnet_index(&self) -> u32;
}

/// Production release backed by hyphae-core's RAII [`VmNetworkResources`].
///
/// `release` simply drops the resources: `VmNetworkResources::drop`
/// removes the NAT rules BEFORE the TAP device is deleted (correct
/// ordering — rules reference the interface).
struct SystemNetworkRelease(hyphae_core::net::VmNetworkResources);

impl NetworkRelease for SystemNetworkRelease {
    fn release(self: Box<Self>) {
        drop(self);
    }

    fn tap_name(&self) -> &str {
        &self.0.allocation.tap_name
    }

    fn subnet_index(&self) -> u32 {
        self.0.allocation.index
    }
}

/// Owns one invocation's network resources for its entire lifecycle.
///
/// Previously the worker leaked `VmNetworkResources` via
/// `std::mem::forget` — five completed invocations left five TAP devices
/// plus their ACCEPT/DROP/MASQUERADE rules on the host. `NetworkScope`
/// keeps the resources alive while the VM runs and releases them exactly
/// once on every terminal path:
///
/// - [`NetworkScope::teardown`] — the deterministic, awaited release used
///   by every terminal path (launch failure, vsock-delivery failure, sync
///   completion, async completion, stop invocation).
/// - `Drop` — a best-effort safety net for paths that skip `teardown`
///   (early returns, panics): releases TAP/NAT synchronously and returns
///   the subnet index via `try_lock`.
///
/// Both paths are idempotent — the release handle is taken out, so a
/// terminal path racing `stop_invocation` cannot double-clean.
pub(crate) struct NetworkScope {
    release: Option<Box<dyn NetworkRelease>>,
    allocator: Arc<tokio::sync::Mutex<SubnetAllocator>>,
}

impl std::fmt::Debug for NetworkScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkScope")
            .field("tap", &self.release.as_ref().map(|r| r.tap_name()))
            .field("released", &self.release.is_none())
            .finish()
    }
}

impl NetworkScope {
    /// Allocate TAP + subnet + NAT rules for a new invocation.
    ///
    /// Returns the [`hyphae_core::launch::NetworkConfig`] for the launch
    /// request plus the scope (wrapped for sharing between the state map,
    /// the async monitor task, and `stop_invocation`) that must be
    /// released on a terminal path.
    async fn setup(
        allocator: Arc<tokio::sync::Mutex<SubnetAllocator>>,
        lan_allow: &[hyphae_core::net::LanAllow],
    ) -> Result<
        (
            hyphae_core::launch::NetworkConfig,
            Arc<tokio::sync::Mutex<Self>>,
        ),
        hyphae_errors::NetworkError,
    > {
        let resources = {
            let mut guard = allocator.lock().await;
            hyphae_core::net::setup_vm_network(&mut guard, true, lan_allow)?
        };
        let net_config = hyphae_core::launch::NetworkConfig {
            tap_name: resources.allocation.tap_name.clone(),
            host_ip: resources.allocation.host_ip,
            guest_ip: resources.allocation.guest_ip,
            guest_mac: resources.allocation.mac.clone(),
            enable_nat: false,
        };
        let scope = Self {
            release: Some(Box::new(SystemNetworkRelease(resources))),
            allocator,
        };
        Ok((net_config, Arc::new(tokio::sync::Mutex::new(scope))))
    }

    /// Test constructor with an injectable release recorder.
    #[cfg(test)]
    fn mock(
        release: Box<dyn NetworkRelease>,
        allocator: Arc<tokio::sync::Mutex<SubnetAllocator>>,
    ) -> Arc<tokio::sync::Mutex<Self>> {
        Arc::new(tokio::sync::Mutex::new(Self {
            release: Some(release),
            allocator,
        }))
    }

    /// Deterministic terminal release: remove NAT rules + TAP device and
    /// return the subnet index to the allocator. Idempotent — a second
    /// call is a no-op.
    pub(crate) async fn teardown(&mut self) {
        if let Some(release) = self.release.take() {
            let tap = release.tap_name().to_owned();
            let index = release.subnet_index();
            release.release();
            self.allocator.lock().await.release(index);
            tracing::info!(
                tap = %tap,
                subnet_index = index,
                "worker: VM network resources released"
            );
        }
    }
}

impl Drop for NetworkScope {
    /// Safety net for paths that skip [`NetworkScope::teardown`]. The
    /// subnet index is returned best-effort: if the allocator lock is
    /// contended the index stays reserved (worst case: one fewer /30
    /// subnet until worker restart) instead of blocking or double-
    /// releasing in `drop`.
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let tap = release.tap_name().to_owned();
            let index = release.subnet_index();
            release.release();
            match self.allocator.try_lock() {
                Ok(mut allocator) => allocator.release(index),
                Err(_) => tracing::warn!(
                    tap = %tap,
                    subnet_index = index,
                    "worker: NetworkScope dropped with allocator busy; \
                     subnet index stays reserved until worker restart"
                ),
            }
            tracing::info!(tap = %tap, "worker: VM network resources released on drop");
        }
    }
}

/// Release an invocation's network resources (idempotent, exactly once).
async fn release_network(network: Option<&Arc<tokio::sync::Mutex<NetworkScope>>>) {
    if let Some(scope) = network {
        scope.lock().await.teardown().await;
    }
}

async fn teardown_vm(pid: u32, vm_id: &str, jail_root: Option<&Path>, runtime_dir: &Path) {
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;

    match i32::try_from(pid) {
        Ok(raw_pid) => {
            if let Err(e) = signal::kill(Pid::from_raw(raw_pid), Signal::SIGKILL) {
                tracing::warn!(pid, vm_id, error = %e, "worker: SIGKILL failed during teardown");
            }
        }
        Err(e) => {
            tracing::warn!(pid, vm_id, error = %e, "worker: invalid PID during teardown");
        }
    }

    if let Err(e) = hyphae_core::jail::remove_cgroup(vm_id) {
        tracing::warn!(vm_id, error = %e, "worker: failed to remove cgroup during teardown");
    }

    if let Some(jail_root) = jail_root {
        // `jail_root` is `{chroot_base}/{exec}/{vm_id}/root`, but the jailer
        // also writes state (e.g. the pid file) next to `root/`. Remove the
        // whole per-VM jail directory, matching hyphae's cleanup_jail().
        let vm_dir = jail_root
            .parent()
            .filter(|p| p.file_name().map(|n| n == OsStr::new(vm_id)).unwrap_or(false));
        let jail_target = vm_dir.unwrap_or(jail_root);
        if let Err(e) = tokio::fs::remove_dir_all(jail_target).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    vm_id,
                    path = %jail_target.display(),
                    error = %e,
                    "worker: failed to remove jail directory during teardown"
                );
            }
        }
    }

    if let Err(e) = tokio::fs::remove_dir_all(runtime_dir).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                vm_id,
                path = %runtime_dir.display(),
                error = %e,
                "worker: failed to remove runtime directory during teardown"
            );
        }
    }
}

/// Full resource teardown for a terminal invocation path: kill the VM,
/// remove the cgroup/jail/runtime directory, then release the TAP/NAT
/// rules and return the subnet index.
///
/// Every terminal path with a live VM process (vsock-delivery failure,
/// sync completion, async completion, stop invocation) funnels through
/// this one function. The network release is idempotent, so a terminal
/// path racing `stop_invocation` cannot double-clean.
async fn teardown_invocation(
    pid: u32,
    vm_id: &str,
    jail_root: Option<&Path>,
    runtime_dir: &Path,
    network: Option<&Arc<tokio::sync::Mutex<NetworkScope>>>,
) {
    teardown_vm(pid, vm_id, jail_root, runtime_dir).await;
    release_network(network).await;
}

/// gRPC WorkerService implementation that delegates VM execution to the
/// VmOrchestrationHandler and streams events back to the gateway.
pub struct WorkerServiceImpl {
    /// Manages long-running deployment VMs.
    deployment_manager: Arc<DeploymentVmManager>,
    /// Unique worker identifier (UUID string).
    pub worker_id: String,
    /// Whether this worker is draining (refusing new work).
    pub draining: Arc<AtomicBool>,
    /// Number of VMs currently running on this worker.
    pub running_vms: Arc<AtomicI32>,
    /// Total resource capacity of this worker.
    pub capacity: WorkerResources,
    /// Path to the local hyphae registry for storing pushed bundles.
    pub registry_path: PathBuf,
    /// Orchestration config (kernel path, firecracker bin, timeouts, etc.).
    pub orch_config: OrchestrationConfig,
    /// CID allocator for vsock guest CIDs.
    pub cid_allocator: Arc<CidAllocator>,
    /// Subnet allocator for TAP device IP allocation (used when enable_network is true).
    pub subnet_allocator: Arc<tokio::sync::Mutex<SubnetAllocator>>,
    /// Active invocations keyed by invocation UUID.
    pub invocations: Arc<RwLock<HashMap<Uuid, InvocationState>>>,
    /// Optional Clickhouse client for invocation records (degraded mode when absent).
    pub clickhouse_client: Option<ClickhouseClient>,
    /// Optional MinIO log uploader for invocation stdout/stderr (degraded mode when absent).
    pub log_uploader: Option<LogUploader>,
    /// MinIO bucket name for building log URLs (used even when log_uploader is None).
    pub log_minio_bucket: String,
    /// Producer handle for run events fanned out to CH + Pulsar + broadcast.
    pub event_sink: EventSink,
    /// In-process broadcast registry; gateway SubscribeRunEvents reads from
    /// this when Pulsar is unavailable.
    pub broadcast: RunBroadcast,
}

impl WorkerServiceImpl {
    /// Create a new WorkerServiceImpl with the given handler and resource capacity.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        deployment_manager: Arc<DeploymentVmManager>,
        worker_id: String,
        memory_mb: u64,
        vcpus: u32,
        registry_path: PathBuf,
        orch_config: OrchestrationConfig,
        cid_allocator: Arc<CidAllocator>,
        subnet_allocator: Arc<tokio::sync::Mutex<SubnetAllocator>>,
        clickhouse_client: Option<ClickhouseClient>,
        log_uploader: Option<LogUploader>,
        log_minio_bucket: String,
        event_sink: EventSink,
        broadcast: RunBroadcast,
    ) -> Self {
        Self {
            deployment_manager,
            worker_id,
            draining: Arc::new(AtomicBool::new(false)),
            running_vms: Arc::new(AtomicI32::new(0)),
            capacity: WorkerResources { memory_mb, vcpus },
            registry_path,
            orch_config,
            cid_allocator,
            subnet_allocator,
            invocations: Arc::new(RwLock::new(HashMap::new())),
            clickhouse_client,
            log_uploader,
            log_minio_bucket,
            event_sink,
            broadcast,
        }
    }
}

/// Stream type returned by SubscribeRunEvents. Boxes the broadcast receiver
/// adapter into a Send + 'static stream so tonic accepts it.
pub type RunEventStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<RunEventProto, Status>> + Send + 'static>,
>;

impl From<RunEvent> for RunEventProto {
    fn from(ev: RunEvent) -> Self {
        RunEventProto {
            run_id: ev.run_id.to_string(),
            seq: ev.seq,
            ts_ms: ev.ts_ms,
            kind: ev.kind,
            content_type: ev.content_type,
            body: ev.body.into(),
            attrs: ev.attrs.into_iter().collect(),
            worker_id: ev.worker_id,
            bundle_id: ev.bundle_id,
            user_id: ev.user_id,
        }
    }
}

#[async_trait]
impl WorkerService for WorkerServiceImpl {
    /// Health returns current resource availability and VM count.
    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        let running = self.running_vms.load(Ordering::Relaxed);
        let draining = self.draining.load(Ordering::Relaxed);

        // Rough estimate: assume each VM uses a proportional share of capacity.
        // The gateway tracks actual per-request resource requirements separately.
        let available_memory_mb = self.capacity.memory_mb.saturating_sub(
            (running as u64) * (self.capacity.memory_mb / (self.capacity.vcpus.max(1) as u64 * 2).max(1)),
        );
        let available_vcpus = self.capacity.vcpus.saturating_sub(running as u32);

        Ok(Response::new(HealthResponse {
            available: Some(WorkerResources {
                memory_mb: available_memory_mb,
                vcpus: available_vcpus,
            }),
            running_vms: running,
            draining,
        }))
    }

    /// Drain signals this worker to stop accepting new work.
    async fn drain(
        &self,
        _request: Request<DrainRequest>,
    ) -> Result<Response<DrainResponse>, Status> {
        self.draining.store(true, Ordering::Relaxed);
        tracing::info!(worker_id = %self.worker_id, "worker drain initiated");
        Ok(Response::new(DrainResponse { draining: true }))
    }

    /// StartDeployment launches a long-running deployment VM.
    async fn start_deployment(
        &self,
        request: Request<StartDeploymentRequest>,
    ) -> Result<Response<StartDeploymentResponse>, Status> {
        if self.draining.load(Ordering::Relaxed) {
            return Ok(Response::new(StartDeploymentResponse {
                success: false,
                error: "worker is draining".into(),
                ..Default::default()
            }));
        }

        let req = request.into_inner();
        let deployment_id = Uuid::parse_str(&req.deployment_id)
            .map_err(|e| Status::invalid_argument(format!("invalid deployment_id: {e}")))?;
        let user_id = Uuid::parse_str(&req.user_id)
            .map_err(|e| Status::invalid_argument(format!("invalid user_id: {e}")))?;

        if self.orch_config.require_jail {
            self.orch_config
                .resolve_launch_mode(deployment_id.simple().to_string())
                .map_err(|e| {
                    Status::failed_precondition(format!(
                        "required jail mode is unavailable: {e}"
                    ))
                })?;
        }

        match self
            .deployment_manager
            .start(deployment_id, &req.bundle_id, user_id, req.probe_port as u16)
            .await
        {
            Ok((guest_ip, pid)) => Ok(Response::new(StartDeploymentResponse {
                success: true,
                guest_ip,
                pid,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(StartDeploymentResponse {
                success: false,
                error: e.to_string(),
                ..Default::default()
            })),
        }
    }

    /// StopDeployment kills a running deployment VM.
    async fn stop_deployment(
        &self,
        request: Request<StopDeploymentRequest>,
    ) -> Result<Response<StopDeploymentResponse>, Status> {
        let req = request.into_inner();
        let deployment_id = Uuid::parse_str(&req.deployment_id)
            .map_err(|e| Status::invalid_argument(format!("invalid deployment_id: {e}")))?;

        match self.deployment_manager.stop(deployment_id).await {
            Ok(()) => Ok(Response::new(StopDeploymentResponse {
                success: true,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(StopDeploymentResponse {
                success: false,
                error: e.to_string(),
            })),
        }
    }

    /// GetDeploymentStatus returns current in-memory status of a deployment.
    async fn get_deployment_status(
        &self,
        request: Request<GetDeploymentStatusRequest>,
    ) -> Result<Response<GetDeploymentStatusResponse>, Status> {
        let req = request.into_inner();
        let deployment_id = Uuid::parse_str(&req.deployment_id)
            .map_err(|e| Status::invalid_argument(format!("invalid deployment_id: {e}")))?;

        let guard = self.deployment_manager.running.read().await;
        let (status, pid, guest_ip) = if let Some(state) = guard.get(&deployment_id) {
            ("running".to_string(), state.pid, state.guest_ip.clone())
        } else {
            ("not_found".to_string(), 0, String::new())
        };

        Ok(Response::new(GetDeploymentStatusResponse {
            status,
            pid,
            guest_ip,
        }))
    }

    /// PushBundle receives an ext4 rootfs image from the gateway and stores it
    /// in the worker's local hyphae Registry.
    async fn push_bundle(
        &self,
        request: Request<PushBundleRequest>,
    ) -> Result<Response<PushBundleResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(
            name = %req.name,
            tag = %req.tag,
            content_hash = %req.content_hash,
            size_bytes = req.size_bytes,
            "worker: receiving bundle push from gateway"
        );

        let registry_path = self.registry_path.clone();
        let name = req.name;
        let tag = req.tag;
        let content_hash = req.content_hash;
        let ext4_data = req.ext4_data;
        let size_bytes = req.size_bytes;
        let manifest_json = req.manifest_json;

        let result = tokio::task::spawn_blocking(move || -> Result<i64, String> {
            let registry = hyphae_core::registry::Registry::open(&registry_path)
                .map_err(|e| format!("failed to open registry: {e}"))?;

            // Dedup: skip if this content hash is already stored locally.
            if let Some(existing) = registry.find_by_hash(&content_hash)
                .map_err(|e| format!("failed to check hash: {e}"))?
            {
                tracing::debug!(
                    content_hash = %content_hash,
                    bundle_id = existing.id,
                    "worker: bundle already exists locally, skipping"
                );
                return Ok(existing.id);
            }

            // Store the ext4 file on disk.
            let disk_filename = format!("sha256-{}.ext4", content_hash);
            let disk_path = registry.storage_dir().join(&disk_filename);
            std::fs::write(&disk_path, &ext4_data)
                .map_err(|e| format!("failed to write ext4: {e}"))?;

            let created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            // Parse manifest from the push request to extract env vars, resources, etc.
            let manifest: Option<hyphae_core::manifest::DimensionManifest> =
                if manifest_json.is_empty() {
                    None
                } else {
                    serde_json::from_str(&manifest_json).ok()
                };

            let m_env = manifest.as_ref().and_then(|m| serde_json::to_string(&m.env).ok());
            let m_resources = manifest.as_ref().and_then(|m| serde_json::to_string(&m.resources).ok());
            let m_secrets = manifest.as_ref().and_then(|m| serde_json::to_string(&m.secrets).ok());
            let m_capabilities = manifest.as_ref().and_then(|m| serde_json::to_string(&m.capabilities).ok());
            let m_a2a = manifest.as_ref().and_then(|m| serde_json::to_string(&m.a2a).ok());
            let m_timeout = manifest.as_ref().and_then(|m| m.resources.timeout_secs.map(|t| t as i64));
            let m_volumes = manifest.as_ref().and_then(|m| serde_json::to_string(&m.volumes).ok());
            let m_memory = manifest.as_ref().and_then(|m| m.resources.memory_mb).unwrap_or(256) as i64;
            let m_vcpus = manifest.as_ref().and_then(|m| m.resources.vcpus.map(|v| v as i64)).unwrap_or(2);

            let new_image = hyphae_core::registry::NewImage {
                content_hash,
                name,
                tag,
                size_bytes,
                source_path: "gateway-push".to_string(),
                init_config: None,
                disk_path: disk_path.to_string_lossy().to_string(),
                created_at,
                default_vcpus: m_vcpus,
                default_memory_mib: m_memory,
                owner_id: None, // Worker stores bundles without ownership scoping
                manifest_resources: m_resources,
                manifest_env: m_env,
                manifest_secrets: m_secrets,
                manifest_capabilities: m_capabilities,
                manifest_a2a: m_a2a,
                manifest_timeout_secs: m_timeout,
                manifest_volumes: m_volumes,
            };

            let registered = registry.register_image(&new_image)
                .map_err(|e| format!("failed to register image: {e}"))?;

            Ok(registered.id)
        })
        .await
        .map_err(|e| Status::internal(format!("task panicked: {e}")))?;

        match result {
            Ok(bundle_id) => {
                tracing::info!(bundle_id, "worker: bundle stored successfully");
                Ok(Response::new(PushBundleResponse {
                    success: true,
                    bundle_id,
                    error: String::new(),
                }))
            }
            Err(e) => {
                tracing::error!(error = %e, "worker: bundle push failed");
                Ok(Response::new(PushBundleResponse {
                    success: false,
                    bundle_id: 0,
                    error: e,
                }))
            }
        }
    }

    /// RunInvocation dispatches an invocation in one of two modes:
    /// - sync: launch VM, capture stdout over vsock UDS, return inline.
    /// - async: launch VM, return invocation_id immediately, background task monitors exit.
    async fn run_invocation(
        &self,
        request: Request<RunInvocationRequest>,
    ) -> Result<Response<RunInvocationResponse>, Status> {
        use hyphae_core::config::VsockConfig;
        use hyphae_core::launch::{launch, LaunchConfig};
        use hyphae_core::process::runtime::{create_vm_runtime_dir, runtime_base_dir};
        use hyphae_core::registry::{parse_image_ref, Registry};

        let req = request.into_inner();

        // ── Validate inputs ────────────────────────────────────────────
        let mode = req.mode.as_str();
        if !matches!(mode, "sync" | "async") {
            return Err(Status::invalid_argument(format!(
                "invalid mode '{}': must be 'sync' or 'async'",
                req.mode
            )));
        }

        if req.bundle_id.is_empty() {
            return Err(Status::not_found("bundle_id is required"));
        }

        let invocation_id = Uuid::new_v4();
        let launch_mode = self
            .orch_config
            .resolve_launch_mode(invocation_id.simple().to_string())
            .map_err(|e| {
                Status::failed_precondition(format!("jail mode could not be established: {e}"))
            })?;
        tracing::info!(
            invocation_id = %invocation_id,
            bundle_id = %req.bundle_id,
            mode = %req.mode,
            vsock_port = req.vsock_port,
            timeout_secs = req.timeout_secs,
            "worker: run_invocation starting"
        );

        // ── Resolve bundle from local registry ─────────────────────────
        let bundle_id_owned = req.bundle_id.clone();
        let registry_path = self.registry_path.clone();
        let image = tokio::task::spawn_blocking(move || {
            let registry = Registry::open(&registry_path)
                .map_err(|e| Status::internal(format!("failed to open registry: {e}")))?;
            let (name, tag) = parse_image_ref(&bundle_id_owned);
            registry
                .find_by_name_tag(&name, &tag)
                .map_err(|e| Status::internal(format!("registry error: {e}")))?
                .ok_or_else(|| {
                    Status::not_found(format!(
                        "bundle '{}' not found in worker local registry",
                        bundle_id_owned
                    ))
                })
        })
        .await
        .map_err(|e| Status::internal(format!("spawn_blocking panicked: {e}")))?
        .map_err(|e: Status| e)?;

        // ── Create runtime directory ───────────────────────────────────
        let base = runtime_base_dir();
        let runtime_dir = create_vm_runtime_dir(&base, &invocation_id)
            .map_err(|e| Status::internal(format!("failed to create runtime dir: {e}")))?;

        // ── Allocate guest CID ─────────────────────────────────────────
        let guest_cid = self.cid_allocator.allocate();

        // ── Build MMDS payload with invocation metadata ────────────────
        // The payload sent to the agent is the raw DimensionRequest from the
        // client (role, content, session_id, history, etc). The agent.ts
        // reads this from stdin and parses it directly — no wrapper envelope.
        let invocation_payload = serde_json::from_slice::<serde_json::Value>(&req.payload)
            .unwrap_or(serde_json::Value::Null);

        // ── Build LaunchConfig ─────────────────────────────────────────
        let vsock_uds_path = runtime_dir.join("v.sock");
        // Ensure no stale UDS file exists — Firecracker creates this socket
        // and will fail with EADDRINUSE if it already exists.
        let _ = std::fs::remove_file(&vsock_uds_path);

        // Network setup: allocate TAP + subnet when enable_network is true.
        // The NetworkScope owns the TAP device, NAT rules, and subnet
        // allocation for this invocation and must be released on every
        // terminal path (previously leaked via std::mem::forget).
        let mut network_scope: Option<Arc<tokio::sync::Mutex<NetworkScope>>> = None;
        let network = if self.orch_config.enable_network {
            let (net_config, scope) =
                NetworkScope::setup(self.subnet_allocator.clone(), &self.orch_config.lan_allow)
                    .await
                    .map_err(|e| Status::internal(format!("network setup failed: {e}")))?;
            network_scope = Some(scope);
            tracing::info!(
                invocation_id = %invocation_id,
                tap = %net_config.tap_name,
                guest_ip = %net_config.guest_ip,
                "worker: VM network enabled"
            );
            Some(net_config)
        } else {
            None
        };

        // Parse manifest_env from JSON string to HashMap<String, String>.
        let env_vars = image.manifest_env.as_ref().and_then(|env_json| {
            serde_json::from_str::<HashMap<String, String>>(env_json).ok()
        });

        let launch_cfg = LaunchConfig {
            vm_id: invocation_id.simple().to_string(),
            kernel_path: self.orch_config.kernel_path.clone(),
            firecracker_bin: self.orch_config.firecracker_bin.clone(),
            rootfs_path: PathBuf::from(&image.disk_path),
            mode: launch_mode,
            network,
            vsock: Some(VsockConfig {
                guest_cid,
                uds_path: vsock_uds_path.to_string_lossy().to_string(),
            }),
            env_vars,
            memory_mib: image.default_memory_mib as u64,
            vcpus: image.default_vcpus as u8,
            extra_boot_args: None,
            volume_drive: None,
            mmds_payload: None, // payload delivered over vsock, not MMDS
        };

        // ── Launch the VM ──────────────────────────────────────────────
        let result = match launch(launch_cfg).await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!(
                    invocation_id = %invocation_id,
                    error = %e,
                    "worker: VM launch failed"
                );
                // The launch module already tore down any jail it created
                // on failure; the worker still owns the runtime dir and the
                // network resources. There is no PID yet, so remove the
                // runtime dir directly and release the network scope.
                if let Err(e) = tokio::fs::remove_dir_all(&runtime_dir).await {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            invocation_id = %invocation_id,
                            path = %runtime_dir.display(),
                            error = %e,
                            "worker: failed to remove runtime dir after launch failure"
                        );
                    }
                }
                release_network(network_scope.as_ref()).await;
                return Err(Status::internal(format!("VM launch failed: {e}")));
            }
        };

        let pid = result.pid;
        let vm_id = result.vm_id.clone();
        let jail_root = result.jail_root.clone();
        let vsock_host_path = resolve_vsock_host_path(
            result.vsock_host_path.as_deref(),
            &vsock_uds_path,
        );
        let launch_log_file = result.log_file.clone();
        tracing::info!(
            invocation_id = %invocation_id,
            pid,
            "worker: VM launched successfully"
        );

        // ── Send payload to agent over vsock ─────────────────────────
        // The dimension-agent inside the VM listens on the vsock UDS.
        // We connect and send a length-delimited protobuf Request envelope
        // containing the invocation payload. The agent spawns the entrypoint
        // and bridges IPC.
        let vsock_stream = match Self::send_vsock_payload(
            &vsock_host_path,
            &invocation_id,
            &invocation_payload,
        )
        .await
        {
            Ok(stream) => stream,
            Err(e) => {
                tracing::error!(invocation_id = %invocation_id, error = %e, "vsock payload send failed");
                teardown_invocation(
                    pid,
                    &vm_id,
                    jail_root.as_deref(),
                    &runtime_dir,
                    network_scope.as_ref(),
                )
                .await;
                return Err(Status::internal(format!("vsock payload send failed: {e}")));
            }
        };
        // Keep the vsock stream alive — dropping it closes the connection
        // before the agent can read the payload.

        // ── Track the invocation ───────────────────────────────────────
        let start_instant = std::time::Instant::now();
        let created_at_ms = now_epoch_ms();
        let user_id = req.user_id.clone();

        let state = InvocationState {
            invocation_id,
            bundle_id: req.bundle_id.clone(),
            mode: req.mode.clone(),
            pid,
            runtime_dir: runtime_dir.clone(),
            vm_id: vm_id.clone(),
            jail_root: jail_root.clone(),
            guest_cid,
            user_id: user_id.clone(),
            started_at: start_instant,
            created_at_ms,
            network: network_scope.clone(),
        };
        self.invocations.write().await.insert(invocation_id, state);
        self.running_vms.fetch_add(1, Ordering::Relaxed);

        // ── State emitter for lifecycle events ─────────────────────────
        let state_emitter = Arc::new(StateEmitter::new(
            invocation_id,
            self.worker_id.clone(),
            req.bundle_id.clone(),
            user_id.clone(),
            self.event_sink.clone(),
        ));
        state_emitter.emit(
            "state",
            serde_json::json!({
                "phase": "started",
                "pid": pid,
                "mode": req.mode,
                "bundle_id": req.bundle_id,
            }),
        );
        state_emitter.emit(
            "state",
            serde_json::json!({ "phase": "payload_delivered" }),
        );

        // ── Mode-specific handling ─────────────────────────────────────
        match mode {
            "sync" => {
                // Sync mode: stream framed OutboundMessage envelopes from
                // the agent. Each one fans into the EventSink; we also
                // accumulate `stdout-final` bodies so we can return the
                // canonical final stdout inline.
                let timeout = if req.timeout_secs > 0 {
                    std::time::Duration::from_secs(req.timeout_secs as u64)
                } else {
                    std::time::Duration::from_secs(300)
                };

                let read_fut = Self::read_envelope_stream(
                    vsock_stream,
                    invocation_id,
                    &self.worker_id,
                    &req.bundle_id,
                    &user_id,
                    &self.event_sink,
                );

                let (stdout_bytes, done) = match tokio::time::timeout(timeout, read_fut).await {
                    Ok(result) => result,
                    Err(_) => {
                        tracing::warn!(
                            invocation_id = %invocation_id,
                            "worker: envelope stream read timed out"
                        );
                        (Vec::new(), None)
                    }
                };

                // Wait for the process to exit (best-effort, short timeout).
                let process_exit_code = Self::wait_for_process_exit(
                    pid,
                    std::time::Duration::from_secs(5),
                )
                .await;

                state_emitter.emit(
                    "state",
                    serde_json::json!({
                        "phase": "process_exited",
                        "exit_code": process_exit_code,
                    }),
                );

                // ── Observability: fire-and-forget CH insert + MinIO upload ──
                let duration_ms = start_instant.elapsed().as_millis() as u64;
                let (status, exit_code) = invocation_outcome(done.as_ref(), process_exit_code);
                let success = status == InvocationStatus::Completed;
                let error = if success {
                    String::new()
                } else if done.is_some() {
                    format!("guest exited with code {exit_code}")
                } else {
                    "agent stream closed without Done".to_string()
                };
                let log_url = LogUploader::log_url(&self.log_minio_bucket, &invocation_id);
                let record = InvocationRecord {
                    invocation_id,
                    user_id: user_id.clone(),
                    bundle_id: req.bundle_id.clone(),
                    worker_id: self.worker_id.clone(),
                    mode: req.mode.clone(),
                    status: status.to_string(),
                    exit_code,
                    duration_ms,
                    log_url,
                    created_at: created_at_ms,
                    completed_at: now_epoch_ms(),
                };

                if let Some(ch) = self.clickhouse_client.clone() {
                    let rec = record.clone();
                    tokio::spawn(async move {
                        if let Err(e) = ch.insert_invocation_record(&rec).await {
                            tracing::warn!(
                                invocation_id = %rec.invocation_id,
                                error = %e,
                                "failed to insert invocation record to Clickhouse"
                            );
                        }
                    });
                }

                if let Some(uploader) = self.log_uploader.clone() {
                    let log_data = stdout_bytes.clone();
                    tokio::spawn(async move {
                        if let Err(e) = uploader.upload_log(&invocation_id, log_data).await {
                            tracing::warn!(
                                invocation_id = %invocation_id,
                                error = %e,
                                "failed to upload invocation log to MinIO"
                            );
                        }
                    });
                }

                // Clean up: remove from invocations, decrement running VMs.
                self.invocations.write().await.remove(&invocation_id);
                self.running_vms.fetch_sub(1, Ordering::Relaxed);
                teardown_invocation(
                    pid,
                    &vm_id,
                    jail_root.as_deref(),
                    &runtime_dir,
                    network_scope.as_ref(),
                )
                .await;

                state_emitter.emit(
                    "state",
                    serde_json::json!({
                        "phase": "completed",
                        "status": status.to_string(),
                        "exit_code": exit_code,
                        "duration_ms": duration_ms,
                    }),
                );
                // Keep the broadcast alive briefly so late subscribers can
                // still receive the terminal events before it's dropped.
                let broadcast_for_finalize = self.broadcast.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    broadcast_for_finalize.finish(invocation_id);
                });

                tracing::info!(
                    invocation_id = %invocation_id,
                    stdout_len = stdout_bytes.len(),
                    exit_code,
                    duration_ms,
                    "worker: sync invocation complete"
                );

                Ok(Response::new(RunInvocationResponse {
                    success,
                    invocation_id: invocation_id.to_string(),
                    stdout: stdout_bytes,
                    exit_code,
                    error,
                }))
            }
            "async" => {
                // Async mode: return immediately, spawn background task to
                // drain the agent envelope stream into the EventSink and
                // monitor the VM process until it exits.
                let invocations = self.invocations.clone();
                let running_vms = self.running_vms.clone();
                let ch_client = self.clickhouse_client.clone();
                let log_up = self.log_uploader.clone();
                let worker_id = self.worker_id.clone();
                let bundle_id = req.bundle_id.clone();
                let mode_str = req.mode.clone();
                let bucket = self.log_minio_bucket.clone();
                let event_sink = self.event_sink.clone();
                let broadcast = self.broadcast.clone();
                let state_emitter_clone = state_emitter.clone();

                tokio::spawn(async move {
                    tracing::info!(
                        invocation_id = %invocation_id,
                        pid,
                        "worker: async invocation monitor started"
                    );

                    // Drain the envelope stream in the background. Returns
                    // when the agent sends Done or the stream closes.
                    let (_stdout, done) = Self::read_envelope_stream(
                        vsock_stream,
                        invocation_id,
                        &worker_id,
                        &bundle_id,
                        &user_id,
                        &event_sink,
                    )
                    .await;

                    // Poll until the process exits.
                    let process_exit_code = Self::wait_for_process_exit(
                        pid,
                        std::time::Duration::from_secs(3600),
                    )
                    .await;
                    state_emitter_clone.emit(
                        "state",
                        serde_json::json!({
                            "phase": "process_exited",
                            "exit_code": process_exit_code,
                        }),
                    );
                    let duration_ms = start_instant.elapsed().as_millis() as u64;

                    // ── Observability ────────────────────────────────────
                    let (status, exit_code) =
                        invocation_outcome(done.as_ref(), process_exit_code);
                    let log_url = LogUploader::log_url(&bucket, &invocation_id);
                    let record = InvocationRecord {
                        invocation_id,
                        user_id,
                        bundle_id,
                        worker_id,
                        mode: mode_str,
                        status: status.to_string(),
                        exit_code,
                        duration_ms,
                        log_url,
                        created_at: created_at_ms,
                        completed_at: now_epoch_ms(),
                    };

                    if let Some(ch) = ch_client {
                        let rec = record.clone();
                        tokio::spawn(async move {
                            if let Err(e) = ch.insert_invocation_record(&rec).await {
                                tracing::warn!(
                                    invocation_id = %rec.invocation_id,
                                    error = %e,
                                    "failed to insert async invocation record to Clickhouse"
                                );
                            }
                        });
                    }

                    // For async mode, upload the Firecracker log file if available.
                    if let Some(uploader) = log_up {
                        let log_data = if let Some(ref lf) = launch_log_file {
                            tokio::fs::read(lf).await.unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        if !log_data.is_empty() {
                            tokio::spawn(async move {
                                if let Err(e) = uploader.upload_log(&invocation_id, log_data).await {
                                    tracing::warn!(
                                        invocation_id = %invocation_id,
                                        error = %e,
                                        "failed to upload async invocation log to MinIO"
                                    );
                                }
                            });
                        }
                    }

                    // Clean up.
                    invocations.write().await.remove(&invocation_id);
                    running_vms.fetch_sub(1, Ordering::Relaxed);
                    teardown_invocation(
                        pid,
                        &vm_id,
                        jail_root.as_deref(),
                        &runtime_dir,
                        network_scope.as_ref(),
                    )
                    .await;

                    state_emitter_clone.emit(
                        "state",
                        serde_json::json!({
                            "phase": "completed",
                            "status": record.status,
                            "exit_code": exit_code,
                            "duration_ms": duration_ms,
                        }),
                    );
                    // Keep the broadcast alive briefly so late subscribers
                    // can still receive the terminal events.
                    let broadcast_for_finalize = broadcast.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        broadcast_for_finalize.finish(invocation_id);
                    });

                    tracing::info!(
                        invocation_id = %invocation_id,
                        exit_code,
                        duration_ms,
                        "worker: async invocation completed and cleaned up"
                    );
                });

                Ok(Response::new(RunInvocationResponse {
                    success: true,
                    invocation_id: invocation_id.to_string(),
                    stdout: Vec::new(),
                    exit_code: 0,
                    error: String::new(),
                }))
            }
            _ => unreachable!("mode was validated above"),
        }
    }

    type SubscribeRunEventsStream = RunEventStream;

    async fn subscribe_run_events(
        &self,
        request: Request<SubscribeRunEventsRequest>,
    ) -> Result<Response<Self::SubscribeRunEventsStream>, Status> {
        let req = request.into_inner();
        let run_id = Uuid::parse_str(&req.run_id)
            .map_err(|e| Status::invalid_argument(format!("invalid run_id: {e}")))?;

        use futures::StreamExt;
        let rx = self.broadcast.subscribe(run_id);
        let stream = tokio_stream::wrappers::BroadcastStream::new(rx).map(|item| match item {
            Ok(ev) => Ok(RunEventProto::from(ev)),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                Err(Status::data_loss(format!("broadcast lagged by {n}")))
            }
        });
        Ok(Response::new(Box::pin(stream)))
    }

    /// StopInvocation kills a running invocation VM by PID and cleans up state.
    async fn stop_invocation(
        &self,
        request: Request<StopInvocationRequest>,
    ) -> Result<Response<StopInvocationResponse>, Status> {
        let req = request.into_inner();
        let invocation_id = Uuid::parse_str(&req.invocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid invocation_id: {e}")))?;

        tracing::info!(
            invocation_id = %invocation_id,
            "worker: stop_invocation requested"
        );

        let state = self
            .invocations
            .write()
            .await
            .remove(&invocation_id);

        match state {
            Some(state) => {
                // ── Observability: record stopped invocation ────────────
                let duration_ms = state.started_at.elapsed().as_millis() as u64;
                let log_url = LogUploader::log_url(&self.log_minio_bucket, &invocation_id);
                let record = InvocationRecord {
                    invocation_id,
                    user_id: state.user_id.clone(),
                    bundle_id: state.bundle_id.clone(),
                    worker_id: self.worker_id.clone(),
                    mode: state.mode.clone(),
                    status: InvocationStatus::Stopped.to_string(),
                    exit_code: -1,
                    duration_ms,
                    log_url,
                    created_at: state.created_at_ms,
                    completed_at: now_epoch_ms(),
                };

                if let Some(ch) = self.clickhouse_client.clone() {
                    let rec = record.clone();
                    tokio::spawn(async move {
                        if let Err(e) = ch.insert_invocation_record(&rec).await {
                            tracing::warn!(
                                invocation_id = %rec.invocation_id,
                                error = %e,
                                "failed to insert stopped invocation record to Clickhouse"
                            );
                        }
                    });
                }

                teardown_invocation(
                    state.pid,
                    &state.vm_id,
                    state.jail_root.as_deref(),
                    &state.runtime_dir,
                    state.network.as_ref(),
                )
                .await;
                self.running_vms.fetch_sub(1, Ordering::Relaxed);

                tracing::info!(
                    invocation_id = %invocation_id,
                    pid = state.pid,
                    duration_ms,
                    "worker: invocation stopped"
                );

                Ok(Response::new(StopInvocationResponse {
                    success: true,
                    error: String::new(),
                }))
            }
            None => {
                tracing::warn!(
                    invocation_id = %invocation_id,
                    "worker: invocation not found for stop request"
                );
                Ok(Response::new(StopInvocationResponse {
                    success: false,
                    error: format!("invocation {} not found", invocation_id),
                }))
            }
        }
    }
}

/// Helper methods for invocation lifecycle management.
impl WorkerServiceImpl {
    /// Decode framed `OutboundMessage` envelopes from the vsock stream until
    /// a `Done` envelope is received or the stream closes. Each message is
    /// converted into a `RunEvent` and pushed to the EventSink. The
    /// concatenated body bytes of any `stdout-final` messages and the received
    /// `Done` payload are returned (stdout is surfaced by the sync RPC).
    async fn read_envelope_stream(
        stream: tokio::net::UnixStream,
        invocation_id: Uuid,
        worker_id: &str,
        bundle_id: &str,
        user_id: &str,
        sink: &EventSink,
    ) -> (Vec<u8>, Option<dimension_protocol::proto::Done>) {
        use dimension_protocol::proto::envelope;
        use dimension_protocol::ProtocolCodec;
        use futures::StreamExt;
        use tokio_util::codec::FramedRead;

        let mut framed = FramedRead::new(stream, ProtocolCodec::default());
        let mut stdout_buf = Vec::new();
        let mut done = None;
        while let Some(frame) = framed.next().await {
            match frame {
                Ok(env) => match env.payload {
                    Some(envelope::Payload::Outbound(msg)) => {
                        let kind = msg.kind.clone();
                        let body = msg.body.clone();
                        let ev = RunEvent::from_outbound(
                            msg,
                            invocation_id,
                            worker_id,
                            bundle_id,
                            user_id,
                        );
                        sink.send(ev);
                        if kind == "stdout-final" {
                            stdout_buf.extend_from_slice(&body);
                        }
                    }
                    Some(envelope::Payload::Done(payload)) => {
                        done = Some(payload);
                        break;
                    }
                    Some(envelope::Payload::Error(err)) => {
                        tracing::warn!(
                            invocation_id = %invocation_id,
                            code = err.code,
                            msg = %err.message,
                            "agent reported error envelope"
                        );
                    }
                    _ => {}
                },
                Err(e) => {
                    tracing::warn!(
                        invocation_id = %invocation_id,
                        error = %e,
                        "vsock envelope decode error"
                    );
                    break;
                }
            }
        }
        (stdout_buf, done)
    }

    /// Send the invocation payload to the dimension-agent over vsock.
    ///
    /// Connects to the Firecracker vsock UDS at `{v.sock}_{port}` (port 1024),
    /// which routes to the guest's vsock listener. Sends a length-delimited
    /// protobuf Request envelope. Retries until the guest agent accepts the
    /// connection (it takes ~1s after VM boot for the agent to bind).
    async fn send_vsock_payload(
        vsock_uds_path: &std::path::Path,
        invocation_id: &Uuid,
        payload: &serde_json::Value,
    ) -> Result<tokio::net::UnixStream, String> {
        use dimension_protocol::{ProtocolCodec, proto::{envelope, Envelope, Request}};
        use tokio_util::codec::Encoder;
        use tokio_util::bytes::{Bytes, BytesMut};
        use tokio::io::AsyncWriteExt;

        // Firecracker vsock: host connects to the UDS, port routing is
        // handled by the vsock protocol internally.
        let vsock_connect_path = vsock_uds_path;

        // Serialize payload to bytes
        let payload_bytes = serde_json::to_vec(payload)
            .map_err(|e| format!("failed to serialize payload: {e}"))?;

        // Build the protobuf envelope
        let envelope = Envelope {
            request_id: invocation_id.to_string(),
            payload: Some(envelope::Payload::Request(Request {
                payload: Bytes::from(payload_bytes),
            })),
        };

        // Encode to length-delimited bytes
        let mut codec = ProtocolCodec::default();
        let mut buf = BytesMut::new();
        codec.encode(envelope, &mut buf)
            .map_err(|e| format!("failed to encode protocol envelope: {e}"))?;

        // Firecracker vsock host→guest: connect to the UDS, send
        // "CONNECT {port}\n", wait for "OK {local_port}\n", then send data.
        // Retry until the guest agent accepts (it takes ~1s after boot).
        use tokio::io::AsyncBufReadExt;
        let port = dimension_protocol::VSOCK_PORT;

        for attempt in 0..300u32 {
            let result: Result<tokio::net::UnixStream, String> = async {
                let mut stream = tokio::net::UnixStream::connect(vsock_connect_path).await
                    .map_err(|e| format!("connect: {e}"))?;

                // Send CONNECT handshake
                let handshake = format!("CONNECT {}\n", port);
                stream.write_all(handshake.as_bytes()).await
                    .map_err(|e| format!("handshake write: {e}"))?;

                // Read response line: "OK <local_port>\n"
                let mut reader = tokio::io::BufReader::new(&mut stream);
                let mut response = String::new();
                reader.read_line(&mut response).await
                    .map_err(|e| format!("handshake read: {e}"))?;

                if !response.starts_with("OK ") {
                    return Err(format!("vsock handshake failed: {response}"));
                }

                // Send the protobuf payload
                stream.write_all(&buf).await
                    .map_err(|e| format!("payload write: {e}"))?;

                tracing::info!(
                    invocation_id = %invocation_id,
                    bytes = buf.len(),
                    attempt,
                    "worker: payload sent to agent over vsock"
                );
                // Return the stream — caller must keep it alive while the
                // agent reads and processes the request.
                Ok(stream)
            }.await;

            match result {
                Ok(stream) => return Ok(stream),
                Err(e) if attempt < 299 => {
                    if attempt % 50 == 0 {
                        tracing::info!(attempt, error = %e, "vsock delivery: waiting for guest agent...");
                    } else {
                        tracing::trace!(attempt, error = %e, "vsock delivery attempt failed, retrying");
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                Err(e) => {
                    return Err(format!(
                        "vsock payload delivery to {} port {} failed after 300 retries (60s): {e}",
                        vsock_connect_path.display(), port
                    ));
                }
            }
        }

        Err("vsock payload delivery: unreachable".to_string())
    }

    /// Wait for a process to exit by polling /proc/<pid>.
    ///
    /// Returns the exit code (0 if the process vanished, -1 on timeout).
    async fn wait_for_process_exit(pid: u32, max_wait: std::time::Duration) -> i32 {
        let deadline = tokio::time::Instant::now() + max_wait;
        let proc_path = format!("/proc/{pid}");

        loop {
            if !std::path::Path::new(&proc_path).exists() {
                return 0;
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(pid, "worker: process exit wait timed out");
                return -1;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_vsock_host_path_prefers_reported_path_and_falls_back() {
        let reported = Path::new("/jail/root/v.sock");
        let fallback = Path::new("/runtime/v.sock");

        assert_eq!(
            resolve_vsock_host_path(Some(reported), fallback),
            reported
        );
        assert_eq!(resolve_vsock_host_path(None, fallback), fallback);
    }

    #[test]
    fn invocation_outcome_uses_guest_done_status() {
        use dimension_protocol::proto::Done;

        let completed = Done {
            metadata: Default::default(),
            exit_code: 0,
            success: true,
        };
        let failed = Done {
            metadata: Default::default(),
            exit_code: 3,
            success: false,
        };
        let inconsistent = Done {
            metadata: Default::default(),
            exit_code: 0,
            success: false,
        };

        assert_eq!(
            invocation_outcome(Some(&completed), 9),
            (InvocationStatus::Completed, 0)
        );
        assert_eq!(
            invocation_outcome(Some(&failed), 0),
            (InvocationStatus::Failed, 3)
        );
        assert_eq!(
            invocation_outcome(Some(&inconsistent), 0),
            (InvocationStatus::Failed, 0)
        );
        assert_eq!(
            invocation_outcome(None, 0),
            (InvocationStatus::Failed, -1)
        );
    }

    #[tokio::test]
    async fn envelope_stream_returns_done_payload() {
        use dimension_protocol::proto::{envelope, Done, Envelope};
        use dimension_protocol::ProtocolCodec;
        use tokio::io::AsyncWriteExt;
        use tokio_util::bytes::BytesMut;
        use tokio_util::codec::Encoder;

        let (reader, mut writer) = tokio::net::UnixStream::pair().expect("create stream pair");
        let expected = Done {
            metadata: Default::default(),
            exit_code: 7,
            success: false,
        };
        let envelope = Envelope {
            request_id: "request-id".to_string(),
            payload: Some(envelope::Payload::Done(expected.clone())),
        };
        let mut encoded = BytesMut::new();
        ProtocolCodec::default()
            .encode(envelope, &mut encoded)
            .expect("encode Done envelope");
        writer.write_all(&encoded).await.expect("write envelope");
        writer.shutdown().await.expect("close writer");

        let fanout = crate::events::EventFanout::spawn(None, None);
        let (stdout, done) = WorkerServiceImpl::read_envelope_stream(
            reader,
            Uuid::new_v4(),
            "worker",
            "bundle",
            "user",
            &fanout.sink,
        )
        .await;

        assert!(stdout.is_empty());
        assert_eq!(done, Some(expected));
    }

    #[tokio::test]
    async fn teardown_vm_removes_jail_and_runtime_directories() {
        let temp = tempfile::tempdir().expect("create temp directory");
        let vm_id = format!("test-{}", Uuid::new_v4());
        // Mirror the real jail layout: {base}/{exec}/{vm_id}/root.
        let vm_dir = temp.path().join("firecracker").join(&vm_id);
        let jail_root = vm_dir.join("root");
        let runtime_dir = temp.path().join("runtime");
        std::fs::create_dir_all(&jail_root).expect("create jail directory");
        std::fs::create_dir_all(&runtime_dir).expect("create runtime directory");
        std::fs::write(jail_root.join("v.sock"), b"sock").expect("write jail artifact");
        std::fs::write(vm_dir.join("firecracker.pid"), b"42\n").expect("write pid file");
        std::fs::write(runtime_dir.join("artifact"), b"runtime")
            .expect("write runtime artifact");

        teardown_vm(u32::MAX, &vm_id, Some(&jail_root), &runtime_dir).await;

        assert!(!vm_dir.exists(), "whole per-VM jail directory must be removed");
        assert!(!jail_root.exists());
        assert!(!runtime_dir.exists());
    }

    #[tokio::test]
    async fn teardown_vm_accepts_missing_paths_and_no_jail() {
        let temp = tempfile::tempdir().expect("create temp directory");
        let runtime_dir = temp.path().join("missing-runtime");

        teardown_vm(u32::MAX, &format!("test-{}", Uuid::new_v4()), None, &runtime_dir).await;
    }

    // ── Network lifecycle tests ────────────────────────────────────

    /// Records releases; shared with the (consumed) mock so assertions
    /// survive the Box being dropped.
    struct MockNetworkRelease {
        tap: String,
        index: u32,
        releases: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl NetworkRelease for MockNetworkRelease {
        fn release(self: Box<Self>) {
            self.releases.fetch_add(1, Ordering::SeqCst);
        }
        fn tap_name(&self) -> &str {
            &self.tap
        }
        fn subnet_index(&self) -> u32 {
            self.index
        }
    }

    fn mock_allocator() -> Arc<tokio::sync::Mutex<SubnetAllocator>> {
        Arc::new(tokio::sync::Mutex::new(SubnetAllocator::new()))
    }

    fn mock_scope(
        index: u32,
        allocator: Arc<tokio::sync::Mutex<SubnetAllocator>>,
    ) -> (
        Arc<tokio::sync::Mutex<NetworkScope>>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let releases = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let scope = NetworkScope::mock(
            Box::new(MockNetworkRelease {
                tap: format!("hyphae-tap{index}"),
                index,
                releases: releases.clone(),
            }),
            allocator,
        );
        (scope, releases)
    }

    #[tokio::test]
    async fn network_teardown_releases_tap_nat_and_subnet_index() {
        let allocator = mock_allocator();
        let (scope, releases) = mock_scope(7, allocator.clone());

        scope.lock().await.teardown().await;

        assert_eq!(releases.load(Ordering::SeqCst), 1);
        assert!(!allocator.lock().await.is_allocated(7));
    }

    #[tokio::test]
    async fn network_teardown_is_idempotent_no_double_cleanup() {
        let allocator = mock_allocator();
        let (scope, releases) = mock_scope(0, allocator.clone());

        // A terminal path racing stop_invocation: both call teardown.
        scope.lock().await.teardown().await;
        scope.lock().await.teardown().await;
        // And the safety-net Drop runs afterwards when the last handle
        // goes away — it must find nothing left to release.
        drop(scope);

        assert_eq!(releases.load(Ordering::SeqCst), 1);
        assert!(!allocator.lock().await.is_allocated(0));
    }

    #[tokio::test]
    async fn network_drop_safety_net_releases_without_explicit_teardown() {
        // Early returns and panics that skip teardown() still release the
        // TAP/NAT resources and (best-effort, uncontended here) the index.
        let allocator = mock_allocator();
        let (scope, releases) = mock_scope(3, allocator.clone());

        drop(scope);

        assert_eq!(releases.load(Ordering::SeqCst), 1);
        assert!(!allocator.lock().await.is_allocated(3));
    }

    #[tokio::test]
    async fn release_network_handles_none_and_is_idempotent() {
        // Networking disabled: no-op.
        release_network(None).await;

        let allocator = mock_allocator();
        let (scope, releases) = mock_scope(5, allocator.clone());
        release_network(Some(&scope)).await;
        release_network(Some(&scope)).await;

        assert_eq!(releases.load(Ordering::SeqCst), 1);
        assert!(!allocator.lock().await.is_allocated(5));
    }

    #[tokio::test]
    async fn teardown_invocation_cleans_dirs_and_releases_network() {
        // The shared terminal-path helper: vsock-delivery failure, sync
        // completion, async completion, and stop_invocation all route
        // through this. Proves the VM teardown AND the network release
        // happen together, exactly once.
        let temp = tempfile::tempdir().expect("create temp directory");
        let vm_id = format!("test-{}", Uuid::new_v4());
        let runtime_dir = temp.path().join("runtime");
        std::fs::create_dir_all(&runtime_dir).expect("create runtime directory");

        let allocator = mock_allocator();
        let (scope, releases) = mock_scope(2, allocator.clone());

        teardown_invocation(u32::MAX, &vm_id, None, &runtime_dir, Some(&scope)).await;

        assert!(!runtime_dir.exists());
        assert_eq!(releases.load(Ordering::SeqCst), 1);
        assert!(!allocator.lock().await.is_allocated(2));

        // A second call (terminal path racing stop) must not double-clean.
        teardown_invocation(u32::MAX, &vm_id, None, &runtime_dir, Some(&scope)).await;

        assert_eq!(releases.load(Ordering::SeqCst), 1);
    }
}
