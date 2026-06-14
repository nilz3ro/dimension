//! VmProcess handle for interacting with a spawned Firecracker process.
//!
//! Provides accessors for process state, PID, paths, and methods for
//! waiting on process exit, API socket readiness, shutdown, and cleanup.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hyphae_errors::ProcessError;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::runtime::cleanup_runtime_dir_sync;
use super::shutdown::{shutdown_escalation, wait_for_exit};
use super::types::{ExitInfo, ProcessState, ShutdownConfig, ShutdownReport, ShutdownStage};

/// Handle to a spawned Firecracker VM process.
///
/// Provides accessors for process metadata, state observation, shutdown
/// escalation, force-stop, and RAII cleanup via `Drop`.
///
/// **Drop behavior:** If not detached, sends synchronous SIGKILL and cleans
/// up the runtime directory. No async operations in Drop.
pub struct VmProcess {
    pub(crate) vm_id: Uuid,
    pub(crate) child_pid: u32,
    pub(crate) state_rx: watch::Receiver<ProcessState>,
    // Kept alive to prevent channel close -- monitor_task holds a clone.
    pub(crate) state_tx: watch::Sender<ProcessState>,
    // Held to keep the monitor task alive; used by shutdown escalation.
    #[allow(dead_code)]
    pub(crate) monitor_handle: JoinHandle<()>,
    pub(crate) runtime_dir: PathBuf,
    pub(crate) api_socket_path: PathBuf,
    pub(crate) pid_file_path: PathBuf,
    pub(crate) log_file: Option<PathBuf>,
    pub(crate) detached: bool,
}

impl VmProcess {
    /// Returns the child process PID.
    pub fn pid(&self) -> u32 {
        self.child_pid
    }

    /// Returns the VM identifier.
    pub fn vm_id(&self) -> &Uuid {
        &self.vm_id
    }

    /// Returns the path to the Firecracker API socket.
    pub fn api_socket_path(&self) -> &Path {
        &self.api_socket_path
    }

    /// Returns the per-VM runtime directory.
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    /// Returns the path to the PID file.
    pub fn pid_file_path(&self) -> &Path {
        &self.pid_file_path
    }

    /// Returns the path to the console log file, if any.
    pub fn log_file(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    /// Returns true if the process is in `Starting` or `Running` state.
    pub fn is_running(&self) -> bool {
        matches!(
            *self.state_rx.borrow(),
            ProcessState::Starting | ProcessState::Running
        )
    }

    /// Returns the current process state snapshot.
    pub fn state(&self) -> ProcessState {
        self.state_rx.borrow().clone()
    }

    /// Waits until the process exits and returns the exit info.
    ///
    /// If the channel closes unexpectedly (monitor task panicked), returns
    /// an ExitInfo with None fields.
    pub async fn wait(&mut self) -> ExitInfo {
        loop {
            {
                let state = self.state_rx.borrow_and_update().clone();
                if let ProcessState::Exited(info) = state {
                    return info;
                }
            }
            if self.state_rx.changed().await.is_err() {
                // Channel closed unexpectedly
                return ExitInfo {
                    exit_code: None,
                    signal: None,
                    timestamp: Instant::now(),
                };
            }
        }
    }

    /// Polls the API socket path until a connection succeeds, or times out.
    ///
    /// Uses 50ms backoff between connection attempts. Returns
    /// `Err(ProcessError::WaitReadyTimeout)` if the socket never becomes ready
    /// within the given timeout.
    pub async fn wait_ready(&self, timeout: Duration) -> Result<(), ProcessError> {
        let result = tokio::time::timeout(timeout, async {
            loop {
                match tokio::net::UnixStream::connect(&self.api_socket_path).await {
                    Ok(_stream) => return,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        })
        .await;

        match result {
            Ok(()) => Ok(()),
            Err(_elapsed) => Err(ProcessError::WaitReadyTimeout),
        }
    }

    /// Returns an additional watch receiver for external state observers.
    pub fn subscribe(&self) -> watch::Receiver<ProcessState> {
        self.state_tx.subscribe()
    }

    /// Detach the VM so it outlives this handle.
    ///
    /// When detached, Drop will not kill the process or clean up runtime dir.
    pub fn detach(&mut self) {
        self.detached = true;
    }

    /// Graceful shutdown with escalation: SendCtrlAltDel -> SIGTERM -> SIGKILL.
    ///
    /// After the process stops, cleans up the runtime directory.
    /// Returns a [`ShutdownReport`] indicating which stage stopped the process.
    pub async fn shutdown(&mut self, config: ShutdownConfig) -> ShutdownReport {
        let report = shutdown_escalation(
            self.child_pid,
            &self.api_socket_path,
            &mut self.state_rx,
            config,
        )
        .await;

        cleanup_runtime_dir_sync(&self.runtime_dir);
        report
    }

    /// Convenience method: shutdown with default configuration.
    pub async fn shutdown_default(&mut self) -> ShutdownReport {
        self.shutdown(ShutdownConfig::default()).await
    }

    /// Immediately kill the process with SIGKILL, wait, and clean up.
    ///
    /// Returns a [`ShutdownReport`] with `ShutdownStage::Sigkill`.
    pub async fn force_stop(&mut self) -> ShutdownReport {
        let start = Instant::now();
        let nix_pid = Pid::from_raw(self.child_pid as i32);

        let _ = signal::kill(nix_pid, Signal::SIGKILL);
        let _ = wait_for_exit(&mut self.state_rx, Duration::from_secs(5)).await;

        cleanup_runtime_dir_sync(&self.runtime_dir);

        ShutdownReport {
            final_stage: ShutdownStage::Sigkill,
            duration: start.elapsed(),
        }
    }
}

impl Drop for VmProcess {
    fn drop(&mut self) {
        if self.detached {
            return;
        }

        // Synchronous SIGKILL -- no async, no block_on, no tokio runtime
        let nix_pid = Pid::from_raw(self.child_pid as i32);
        let _ = signal::kill(nix_pid, Signal::SIGKILL);

        // Synchronous cleanup
        cleanup_runtime_dir_sync(&self.runtime_dir);

        tracing::trace!(pid = self.child_pid, "drop: sent SIGKILL and cleaned up");
    }
}
