//! Stop pipeline: look up VM -> shutdown process -> untrack.
//!
//! The stop pipeline looks up a VM in the registry, sends a graceful
//! shutdown to the Firecracker process, and removes the VM tracking record.

use hyphae_errors::{HyphaeError, OrchestratorError};

use crate::process::types::ShutdownConfig;

use super::types::{ActionType, PlannedAction, ProgressFn, StopPlan};
use super::Orchestrator;

impl Orchestrator {
    /// Execute the stop pipeline.
    ///
    /// Steps:
    /// 1. Look up VM in registry
    /// 2. Send shutdown to Firecracker process
    /// 3. Untrack VM in registry
    pub async fn stop(
        &self,
        vm_id: &str,
        progress: Option<ProgressFn>,
    ) -> Result<(), HyphaeError> {
        let emit = |msg: &str| {
            if let Some(ref cb) = progress {
                cb(msg);
            }
        };

        // 1. Look up VM in registry to verify it exists.
        emit(&format!("looking up VM {vm_id}"));
        let all_vms = self
            .registry
            .list_all_vms()
            .map_err(|e| OrchestratorError::RegistryFailed(e.to_string()))?;

        let vm_record = all_vms
            .iter()
            .find(|v| v.vm_id == vm_id)
            .ok_or_else(|| OrchestratorError::VmNotFound {
                vm_id: vm_id.to_string(),
            })?;

        let _image_id = vm_record.image_id;

        // 2. Attempt to find and kill the process.
        // Read the PID from the runtime directory if available, then
        // perform shutdown escalation. Since we may not have a VmProcess
        // handle (the VM was started in a previous session), we use
        // direct signal-based shutdown.
        emit(&format!("shutting down VM {vm_id}"));

        // Locate the runtime directory for this VM.
        let base = crate::process::runtime::runtime_base_dir();
        let vms_dir = crate::process::vms_dir(&base);
        let runtime_dir = vms_dir.join(vm_id);

        if runtime_dir.exists() {
            let pid_file = runtime_dir.join("firecracker.pid");
            if pid_file.exists() {
                if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
                    if let Ok(pid) = pid_str.trim().parse::<u32>() {
                        let api_socket = runtime_dir.join("firecracker.sock");
                        let config = ShutdownConfig::default();

                        // Use tokio watch channel for shutdown_escalation.
                        let (state_tx, mut state_rx) = tokio::sync::watch::channel(
                            crate::process::types::ProcessState::Running,
                        );

                        // Spawn a task to monitor the process exit.
                        let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
                        let monitor_tx = state_tx.clone();
                        tokio::spawn(async move {
                            // Poll for process exit.
                            loop {
                                match nix::sys::signal::kill(nix_pid, None) {
                                    Ok(()) => {
                                        // Still alive, wait a bit.
                                        tokio::time::sleep(
                                            std::time::Duration::from_millis(100),
                                        )
                                        .await;
                                    }
                                    Err(_) => {
                                        // Process gone.
                                        let _ = monitor_tx.send(
                                            crate::process::types::ProcessState::Exited(
                                                crate::process::types::ExitInfo {
                                                    exit_code: None,
                                                    signal: None,
                                                    timestamp: std::time::Instant::now(),
                                                },
                                            ),
                                        );
                                        break;
                                    }
                                }
                            }
                        });

                        let report = crate::process::shutdown::shutdown_escalation(
                            pid,
                            &api_socket,
                            &mut state_rx,
                            config,
                        )
                        .await;

                        emit(&format!(
                            "shutdown complete (stage: {:?}, duration: {:?})",
                            report.final_stage, report.duration
                        ));
                    }
                }
            }

            // Clean up runtime directory.
            crate::process::cleanup_runtime_dir_sync(&runtime_dir);
        }

        // 3. Untrack VM in registry.
        emit("untracking VM from registry");
        self.registry
            .untrack_vm(vm_id)
            .map_err(|e| OrchestratorError::RegistryFailed(e.to_string()))?;

        emit(&format!("VM {vm_id} stopped"));
        Ok(())
    }

    /// Dry-run: plan what a stop would do without side effects.
    pub fn plan_stop(&self, vm_id: &str) -> Result<StopPlan, HyphaeError> {
        // Verify VM exists.
        let all_vms = self
            .registry
            .list_all_vms()
            .map_err(|e| OrchestratorError::DryRunFailed(e.to_string()))?;

        if !all_vms.iter().any(|v| v.vm_id == vm_id) {
            return Err(OrchestratorError::VmNotFound {
                vm_id: vm_id.to_string(),
            }
            .into());
        }

        let actions = vec![
            PlannedAction {
                action_type: ActionType::Skip,
                description: format!("look up VM {vm_id}"),
            },
            PlannedAction {
                action_type: ActionType::Destroy,
                description: "send graceful shutdown (SendCtrlAltDel -> SIGTERM -> SIGKILL)"
                    .to_string(),
            },
            PlannedAction {
                action_type: ActionType::Destroy,
                description: "clean up runtime directory".to_string(),
            },
            PlannedAction {
                action_type: ActionType::Destroy,
                description: "untrack VM from registry".to_string(),
            },
        ];

        Ok(StopPlan {
            actions,
            vm_id: vm_id.to_string(),
        })
    }
}
