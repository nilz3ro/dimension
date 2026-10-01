//! DeploymentVmManager — long-running VM lifecycle management.
//!
//! Unlike VmOrchestrationHandler (ephemeral VMs cleaned up by VmLifecycleGuard),
//! DeploymentVmManager explicitly starts and stops named persistent VMs.
//!
//! CRITICAL: Deployment VMs use runtime_base/deployments/ NOT runtime_base/vms/.
//! The orphan reaper scans only vms/ — deployments/ is invisible to it.
//! DO NOT use VmLifecycleGuard for deployment VMs — its Drop impl sends SIGKILL.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use uuid::Uuid;

use dimension_store::DeploymentStore;
use hyphae_core::net::SubnetAllocator;

use crate::orchestration::config::{CidAllocator, OrchestrationConfig};

/// State of a running deployment VM tracked in memory.
#[derive(Debug, Clone)]
pub struct RunningDeployment {
    pub deployment_id: Uuid,
    pub pid: u32,
    pub guest_ip: String,
    pub probe_port: u16,
    pub runtime_dir: PathBuf,
    pub vm_id: String,
    pub jail_root: Option<PathBuf>,
}

async fn teardown_deployment(
    pid: Option<u32>,
    vm_id: &str,
    jail_root: Option<&Path>,
    runtime_dir: &Path,
) {
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;

    if let Some(pid) = pid {
        match i32::try_from(pid) {
            Ok(raw_pid) => {
                if let Err(e) = signal::kill(Pid::from_raw(raw_pid), Signal::SIGKILL) {
                    tracing::warn!(pid, vm_id, error = %e, "deployment SIGKILL failed during teardown");
                }
            }
            Err(e) => {
                tracing::warn!(pid, vm_id, error = %e, "deployment has invalid PID during teardown");
            }
        }
    }

    if let Err(e) = hyphae_core::jail::remove_cgroup(vm_id) {
        tracing::warn!(vm_id, error = %e, "failed to remove deployment cgroup during teardown");
    }

    if let Some(jail_root) = jail_root {
        // `jail_root` is `{chroot_base}/{exec}/{vm_id}/root`; the jailer also
        // writes state next to `root/`. Remove the whole per-VM jail dir.
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
                    "failed to remove deployment jail directory during teardown"
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
                "failed to remove deployment runtime directory during teardown"
            );
        }
    }
}

/// Manages the lifecycle of long-running deployment VMs.
///
/// Holds in-memory state for running deployments. Persists status
/// to DeploymentStore so the gateway survives restarts.
pub struct DeploymentVmManager {
    pub store: Option<Arc<dyn DeploymentStore>>,
    /// In-memory map of deployment_id -> running state.
    /// Only contains VMs that are currently alive on this worker.
    pub running: Arc<RwLock<HashMap<Uuid, RunningDeployment>>>,
    /// Orchestration config (kernel path, firecracker bin, etc).
    pub orch_config: OrchestrationConfig,
    /// Allocator for unique guest CIDs.
    pub cid_allocator: Arc<CidAllocator>,
    /// Path to the hyphae bundle registry data directory.
    pub registry_path: std::path::PathBuf,
    /// Shared subnet allocator for TAP device / IP assignment.
    pub subnet_allocator: Arc<Mutex<SubnetAllocator>>,
}

impl DeploymentVmManager {
    /// Create a new DeploymentVmManager with a DeploymentStore (gateway-side).
    pub fn new(
        store: Arc<dyn DeploymentStore>,
        orch_config: OrchestrationConfig,
        cid_allocator: Arc<CidAllocator>,
        registry_path: std::path::PathBuf,
    ) -> Self {
        Self {
            store: Some(store),
            running: Arc::new(RwLock::new(HashMap::new())),
            orch_config,
            cid_allocator,
            registry_path,
            subnet_allocator: Arc::new(Mutex::new(SubnetAllocator::new())),
        }
    }

    /// Create a DeploymentVmManager without a DeploymentStore (worker-side).
    ///
    /// Workers track deployments in memory only — the gateway owns the Postgres store.
    pub fn new_for_worker(
        orch_config: OrchestrationConfig,
        cid_allocator: Arc<CidAllocator>,
        registry_path: std::path::PathBuf,
    ) -> Self {
        Self {
            store: None,
            running: Arc::new(RwLock::new(HashMap::new())),
            orch_config,
            cid_allocator,
            registry_path,
            subnet_allocator: Arc::new(Mutex::new(SubnetAllocator::new())),
        }
    }

    /// Start a deployment VM.
    ///
    /// 1. Resolves bundle from registry
    /// 2. Creates runtime dir under deployments/ (NOT vms/)
    /// 3. Launches Firecracker WITHOUT VmLifecycleGuard
    /// 4. Stores PID + guest_ip in running map
    /// 5. Returns guest_ip + pid on success
    pub async fn start(
        &self,
        deployment_id: Uuid,
        bundle_id: &str,
        _user_id: Uuid,
        probe_port: u16,
    ) -> anyhow::Result<(String, u32)> {
        use hyphae_core::launch::{launch, LaunchConfig, NetworkConfig};
        use hyphae_core::net::setup_vm_network;
        use hyphae_core::process::runtime::{create_deployment_runtime_dir, runtime_base_dir};
        use hyphae_core::registry::{parse_image_ref, Registry};
        use hyphae_core::config::VsockConfig;

        let vm_id = deployment_id.simple().to_string();
        let launch_mode = self
            .orch_config
            .resolve_launch_mode(vm_id.clone())
            .map_err(|e| anyhow::anyhow!("jail mode could not be established: {e}"))?;
        let configured_jail_root = match &launch_mode {
            hyphae_core::launch::LaunchMode::Jailed(config) => Some(config.jail_root()),
            hyphae_core::launch::LaunchMode::Direct => None,
        };

        // Create runtime dir under deployments/ (NOT vms/)
        let base = runtime_base_dir();
        let runtime_dir = create_deployment_runtime_dir(&base, &deployment_id)
            .map_err(|e| anyhow::anyhow!("failed to create deployment runtime dir: {e}"))?;

        // Look up bundle from registry.
        // Use spawn_blocking because Registry/rusqlite is not Send+Sync.
        let bundle_id_owned = bundle_id.to_string();
        let registry_path = self.registry_path.clone();
        let image = tokio::task::spawn_blocking(move || {
            let registry = Registry::open(&registry_path)?;
            let (name, tag) = parse_image_ref(&bundle_id_owned);
            registry
                .find_by_name_tag(&name, &tag)
                .map_err(|e| anyhow::anyhow!("registry error: {e}"))?
                .ok_or_else(|| anyhow::anyhow!("bundle {} not found", bundle_id_owned))
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking panicked: {e}"))??;

        // Network setup (only when network is enabled in orch_config)
        let network = if self.orch_config.enable_network {
            let mut allocator = self.subnet_allocator.lock()
                .map_err(|e| anyhow::anyhow!("subnet allocator lock poisoned: {e}"))?;
            // No NAT for deployment-local VMs: no allowlist to apply.
            let resources = setup_vm_network(&mut allocator, false, &[])
                .map_err(|e| anyhow::anyhow!("network setup failed: {e}"))?;
            // Copy fields before forgetting resources so Drop doesn't teardown the TAP.
            // Deployment VMs are long-lived; TAP cleanup happens on stop().
            let tap_name = resources.allocation.tap_name.clone();
            let host_ip = resources.allocation.host_ip;
            let guest_ip_addr = resources.allocation.guest_ip;
            let mac = resources.allocation.mac.clone();
            std::mem::forget(resources);
            Some(NetworkConfig {
                tap_name,
                host_ip,
                guest_ip: guest_ip_addr,
                guest_mac: mac,
                enable_nat: false,
            })
        } else {
            None
        };

        let guest_ip = network
            .as_ref()
            .map(|n| n.guest_ip.to_string())
            .unwrap_or_default();

        // Guest CID allocation
        let guest_cid = self.cid_allocator.allocate();

        let vsock_uds_path = runtime_dir.join("v.sock");
        let launch_cfg = LaunchConfig {
            vm_id: vm_id.clone(),
            kernel_path: self.orch_config.kernel_path.clone(),
            firecracker_bin: self.orch_config.firecracker_bin.clone(),
            rootfs_path: std::path::PathBuf::from(&image.disk_path),
            mode: launch_mode,
            network,
            vsock: Some(VsockConfig {
                guest_cid,
                uds_path: vsock_uds_path.to_string_lossy().to_string(),
            }),
            env_vars: None,
            memory_mib: image.default_memory_mib as u64,
            vcpus: image.default_vcpus as u8,
            extra_boot_args: None,
            volume_drive: None,
            mmds_payload: None,
        };

        let result = match launch(launch_cfg).await {
            Ok(result) => result,
            Err(e) => {
                teardown_deployment(
                    None,
                    &vm_id,
                    configured_jail_root.as_deref(),
                    &runtime_dir,
                )
                .await;
                return Err(anyhow::anyhow!("launch failed: {e}"));
            }
        };

        let pid = result.pid;

        // Store in running map (NO VmLifecycleGuard)
        let state = RunningDeployment {
            deployment_id,
            pid,
            guest_ip: guest_ip.clone(),
            probe_port,
            runtime_dir,
            vm_id: result.vm_id,
            jail_root: result.jail_root,
        };
        self.running.write().await.insert(deployment_id, state);

        Ok((guest_ip, pid))
    }

    /// Stop a deployment VM by SIGKILL and cleanup.
    ///
    /// Sends SIGKILL to the PID and removes cgroup, jail, and runtime state.
    pub async fn stop(&self, deployment_id: Uuid) -> anyhow::Result<()> {
        let state = self
            .running
            .write()
            .await
            .remove(&deployment_id)
            .ok_or_else(|| anyhow::anyhow!("deployment {} not found in running map", deployment_id))?;

        teardown_deployment(
            Some(state.pid),
            &state.vm_id,
            state.jail_root.as_deref(),
            &state.runtime_dir,
        )
        .await;

        Ok(())
    }

    /// Return current status of a deployment (checks live PID).
    pub async fn get_status(&self, deployment_id: Uuid) -> anyhow::Result<String> {
        use nix::sys::signal;
        use nix::unistd::Pid;

        let guard = self.running.read().await;
        if let Some(state) = guard.get(&deployment_id) {
            let pid = Pid::from_raw(state.pid as i32);
            if signal::kill(pid, None).is_ok() {
                Ok("running".to_string())
            } else {
                Ok("stopped".to_string())
            }
        } else {
            Ok("not_found".to_string())
        }
    }
}
