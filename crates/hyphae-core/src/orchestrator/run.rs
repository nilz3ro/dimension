//! Run pipeline: resolve bundle -> launch VM via unified launch path -> track.
//!
//! The run pipeline resolves an image reference from the registry, builds a
//! [`LaunchConfig`], dispatches to the unified launch module (which handles
//! direct or jailed mode, networking, and boot arguments), and tracks the VM.

use std::path::PathBuf;

use hyphae_errors::{HyphaeError, OrchestratorError};
use tracing::warn;

use crate::jail::{find_jailer, validate_jail_user, JailConfig};
use crate::launch::{launch, LaunchConfig, LaunchMode, NetworkConfig};
use crate::net::{setup_vm_network, SubnetAllocator};
use crate::process::runtime::runtime_base_dir;
use crate::registry::parse_image_ref;

use super::types::{ActionType, PlannedAction, ProgressFn, RunPlan, RunRequest, RunResult};
use super::Orchestrator;

impl Orchestrator {
    /// Execute the run pipeline.
    ///
    /// Steps:
    /// 1. Resolve image reference from registry
    /// 2. Validate kernel path
    /// 3. Determine vCPU and memory settings (request overrides or bundle defaults)
    /// 4. Determine launch mode (direct or jailed)
    /// 5. Set up networking if requested
    /// 6. Build LaunchConfig and call launch()
    /// 7. Track VM in registry
    /// 8. Return RunResult
    pub async fn run(
        &self,
        request: RunRequest,
        progress: Option<ProgressFn>,
    ) -> Result<RunResult, HyphaeError> {
        let emit = |msg: &str| {
            if let Some(ref cb) = progress {
                cb(msg);
            }
        };

        // 1. Resolve image reference.
        let (name, tag) = parse_image_ref(&request.reference);
        emit(&format!("resolving image {name}:{tag}"));

        let image = self
            .registry
            .find_by_name_tag(&name, &tag)
            .map_err(|e| OrchestratorError::RegistryFailed(e.to_string()))?
            .ok_or_else(|| OrchestratorError::BundleNotFound {
                reference: request.reference.clone(),
            })?;

        // 2. Validate kernel path.
        if !request.kernel_path.exists() {
            return Err(OrchestratorError::KernelNotFound {
                path: request.kernel_path,
            }
            .into());
        }

        // 3. Determine settings.
        let vcpus = request.vcpus.unwrap_or(image.default_vcpus as u8);
        let memory_mib = request
            .memory_mib
            .unwrap_or(image.default_memory_mib as u64);
        let reference = format!("{}:{}", image.name, image.tag);

        emit(&format!(
            "configuring VM: {vcpus} vCPUs, {memory_mib} MiB memory"
        ));

        // 4. Determine launch mode.
        let firecracker_bin = which::which("firecracker").map_err(|e| {
            OrchestratorError::SpawnFailed(format!("firecracker binary not found: {e}"))
        })?;

        let launch_mode = if request.jail {
            // Try to set up jailed mode; fall back to direct if jailer/user not available.
            match (
                find_jailer(Some(&firecracker_bin)),
                validate_jail_user(),
            ) {
                (Ok(jailer_bin), Ok(jail_user)) => {
                    emit("using jailed launch mode");
                    LaunchMode::Jailed(JailConfig::new(
                        jailer_bin,
                        firecracker_bin.clone(),
                        String::new(), // vm_id filled by launch module
                        jail_user.uid,
                        jail_user.gid,
                    ))
                }
                (Err(e), _) => {
                    warn!(
                        error = %e,
                        "jailer not available, falling back to direct mode"
                    );
                    emit("jailer not available, falling back to direct launch mode");
                    LaunchMode::Direct
                }
                (_, Err(e)) => {
                    warn!(
                        error = %e,
                        "jail user not available, falling back to direct mode"
                    );
                    emit("jail user not available, falling back to direct launch mode");
                    LaunchMode::Direct
                }
            }
        } else {
            emit("using direct launch mode");
            LaunchMode::Direct
        };

        // 5. Set up networking if requested.
        let network = if request.enable_network {
            emit("setting up VM network (TAP device, subnet allocation)");
            let mut allocator = SubnetAllocator::new();
            let resources = setup_vm_network(&mut allocator, !request.no_nat)?;

            let net_config = NetworkConfig {
                tap_name: resources.allocation.tap_name.clone(),
                host_ip: resources.allocation.host_ip,
                guest_ip: resources.allocation.guest_ip,
                guest_mac: resources.allocation.mac.clone(),
                enable_nat: !request.no_nat,
            };

            // Deliberately leak VmNetworkResources so the TAP device persists
            // while the VM runs. The TAP device's Drop would delete it, but
            // we need it alive. The stop pipeline or recover_orphan_taps()
            // handles cleanup when the VM shuts down.
            std::mem::forget(resources);

            emit(&format!(
                "network ready: TAP={}, guest_ip={}",
                net_config.tap_name, net_config.guest_ip
            ));
            Some(net_config)
        } else {
            None
        };

        // 6. Build LaunchConfig and launch.
        emit("launching Firecracker VM");
        let launch_config = LaunchConfig {
            mode: launch_mode,
            firecracker_bin,
            kernel_path: request.kernel_path.clone(),
            rootfs_path: PathBuf::from(&image.disk_path),
            vcpus,
            memory_mib,
            network,
            vm_id: String::new(), // launch module generates its own VM ID
            extra_boot_args: request.boot_args.clone(),
            vsock: request.vsock.as_ref().map(|v| crate::config::VsockConfig {
                guest_cid: v.guest_cid,
                uds_path: v.uds_path.clone(),
            }),
            // Runtime env vars not yet wired into orchestrator run path;
            // Plan 07-02 will populate this from manifest [env] sections.
            env_vars: None,
            // Phase 17 (Persistent Volumes) will populate this when needed.
            volume_drive: None,
            // MMDS payload not yet wired into orchestrator run path;
            // M003 S01 callers will populate this directly via LaunchConfig.
            mmds_payload: None,
        };

        let result = launch(launch_config).await?;

        let vm_id = result.vm_id.clone();
        let pid = result.pid;

        // Compute API socket path from runtime base dir and vm_id.
        let base = runtime_base_dir();
        let api_socket = base
            .join("vms")
            .join(&vm_id)
            .join("firecracker.sock")
            .to_string_lossy()
            .to_string();

        // 7. Track VM in registry.
        emit("tracking VM in registry");
        if let Err(e) = self.registry.track_vm(&vm_id, image.id) {
            // Note: VM is already running. We log the tracking failure but
            // don't kill the VM -- the user can still manage it by PID.
            warn!(
                vm_id = %vm_id,
                error = %e,
                "failed to track VM in registry (VM is still running)"
            );
            return Err(OrchestratorError::RegistryFailed(e.to_string()).into());
        }

        emit(&format!("VM {vm_id} started (pid={pid})"));

        Ok(RunResult {
            vm_id,
            reference,
            pid,
            api_socket,
            vcpus,
            memory_mib,
            vsock_uds_path: request.vsock.as_ref().map(|v| v.uds_path.clone()),
            log_file: result.log_file,
        })
    }

    /// Dry-run: plan what a run would do without side effects.
    pub fn plan_run(&self, request: RunRequest) -> Result<RunPlan, HyphaeError> {
        // Resolve image reference.
        let (name, tag) = parse_image_ref(&request.reference);
        let image = self
            .registry
            .find_by_name_tag(&name, &tag)
            .map_err(|e| OrchestratorError::DryRunFailed(e.to_string()))?
            .ok_or_else(|| OrchestratorError::BundleNotFound {
                reference: request.reference.clone(),
            })?;

        // Determine settings.
        let vcpus = request.vcpus.unwrap_or(image.default_vcpus as u8);
        let memory_mib = request
            .memory_mib
            .unwrap_or(image.default_memory_mib as u64);
        let reference = format!("{}:{}", image.name, image.tag);
        let launch_mode = if request.jail { "jailed" } else { "direct" };
        let network_enabled = request.enable_network;

        let mut actions = vec![
            PlannedAction {
                action_type: ActionType::Skip,
                description: format!("resolve bundle {reference}"),
            },
            PlannedAction {
                action_type: ActionType::Skip,
                description: "validate kernel path".to_string(),
            },
            PlannedAction {
                action_type: ActionType::Create,
                description: format!(
                    "generate VM config ({vcpus} vCPUs, {memory_mib} MiB)"
                ),
            },
        ];

        if network_enabled {
            actions.push(PlannedAction {
                action_type: ActionType::Create,
                description: format!(
                    "set up networking (TAP device, subnet allocation{})",
                    if request.no_nat { "" } else { ", NAT rules" }
                ),
            });
        }

        if request.vsock.is_some() {
            actions.push(PlannedAction {
                action_type: ActionType::Create,
                description: "configure vsock device".to_string(),
            });
        }

        actions.push(PlannedAction {
            action_type: ActionType::Create,
            description: format!("launch Firecracker ({launch_mode} mode)"),
        });
        actions.push(PlannedAction {
            action_type: ActionType::Create,
            description: "track VM in registry".to_string(),
        });

        // Check kernel path existence for plan warning.
        if !request.kernel_path.exists() {
            actions.insert(
                1,
                PlannedAction {
                    action_type: ActionType::Skip,
                    description: format!(
                        "WARNING: kernel not found at {}",
                        request.kernel_path.display()
                    ),
                },
            );
        }

        Ok(RunPlan {
            actions,
            reference,
            vcpus,
            memory_mib,
            launch_mode: launch_mode.to_string(),
            network_enabled,
            vsock_enabled: request.vsock.is_some(),
        })
    }
}
