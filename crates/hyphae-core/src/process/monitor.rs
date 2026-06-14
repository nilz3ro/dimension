//! Per-VM monitor task that drives `child.wait()` and publishes state
//! transitions via a watch channel.
//!
//! The monitor task is spawned by `spawn()` and runs for the lifetime of
//! the child process. It transitions state from Starting -> Running -> Exited.

use std::time::Instant;

use tokio::process::Child;
use tokio::sync::watch;

use super::types::{ExitInfo, ProcessState};

/// Monitor a child process and publish state transitions.
///
/// This function is intended to be run via `tokio::spawn`. It:
/// 1. Sends `ProcessState::Running` immediately
/// 2. Awaits `child.wait()` to reap the process (prevents zombies)
/// 3. Constructs `ExitInfo` from the exit status
/// 4. Sends `ProcessState::Exited(info)`
pub(crate) async fn monitor_task(
    mut child: Child,
    state_tx: watch::Sender<ProcessState>,
    pid: u32,
) {
    // Transition to Running
    let _ = state_tx.send(ProcessState::Running);

    // Wait for process exit -- this also reaps the zombie
    match child.wait().await {
        Ok(status) => {
            let signal = {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    status.signal()
                }
                #[cfg(not(unix))]
                {
                    None
                }
            };

            let info = ExitInfo {
                exit_code: status.code(),
                signal,
                timestamp: Instant::now(),
            };
            tracing::info!(pid, exit_code = ?info.exit_code, signal = ?info.signal, "process exited");
            let _ = state_tx.send(ProcessState::Exited(info));
        }
        Err(e) => {
            tracing::error!(pid, error = %e, "failed to wait on process");
            let info = ExitInfo {
                exit_code: None,
                signal: None,
                timestamp: Instant::now(),
            };
            let _ = state_tx.send(ProcessState::Exited(info));
        }
    }
}
