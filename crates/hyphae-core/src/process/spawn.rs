//! Process spawning for Firecracker VMs.
//!
//! Creates a per-VM runtime directory, spawns the process, writes a PID file,
//! launches a monitor task, and returns a `VmProcess` handle.

use std::process::Stdio;

use hyphae_errors::ProcessError;
use tokio::sync::watch;
use uuid::Uuid;

use super::handle::VmProcess;
use super::monitor::monitor_task;
use super::runtime::{create_vm_runtime_dir, runtime_base_dir};
use super::types::{ProcessState, SpawnConfig};

/// Spawn a process using the given configuration.
///
/// This function:
/// 1. Resolves the runtime base directory (from config override or environment)
/// 2. Generates a UUID v4 for the VM ID
/// 3. Creates a per-VM runtime directory
/// 4. Constructs and spawns a `tokio::process::Command`
/// 5. Writes a PID file at `{runtime_dir}/firecracker.pid`
/// 6. Creates a watch channel initialized to `ProcessState::Starting`
/// 7. Launches a monitor task via `tokio::spawn`
/// 8. Returns a `VmProcess` handle
///
/// The spawned process uses `Stdio::null()` for stdin, stdout, and stderr.
/// `kill_on_drop` is NOT set -- lifecycle is managed explicitly.
pub async fn spawn(config: &SpawnConfig) -> Result<VmProcess, ProcessError> {
    // 1. Resolve base dir
    let base = config
        .runtime_base_dir
        .clone()
        .unwrap_or_else(runtime_base_dir);

    // 2. Generate VM ID
    let vm_id = Uuid::new_v4();

    // 3. Create runtime directory
    let runtime_dir = create_vm_runtime_dir(&base, &vm_id)?;

    let api_socket_path = runtime_dir.join("firecracker.sock");
    let pid_file_path = runtime_dir.join("firecracker.pid");

    // Determine log file path: use override from config, or default to runtime_dir/console.log.
    let log_file_path = config
        .log_file
        .clone()
        .unwrap_or_else(|| runtime_dir.join("console.log"));

    // 4. Build and spawn command
    let mut cmd = tokio::process::Command::new(&config.firecracker_bin);
    cmd.arg("--api-sock").arg(&api_socket_path);
    if let Some(config_file) = &config.config_file {
        cmd.arg("--config-file").arg(config_file);
    }
    cmd.stdin(Stdio::null());

    // Open log file for stdout/stderr so guest serial console output is captured.
    let log_file = std::fs::File::create(&log_file_path).map_err(|e| {
        ProcessError::SpawnFailed(format!(
            "failed to create log file {}: {}",
            log_file_path.display(),
            e
        ))
    })?;
    let log_file_dup = log_file.try_clone().map_err(|e| {
        ProcessError::SpawnFailed(format!(
            "failed to duplicate log file handle: {}",
            e
        ))
    })?;
    cmd.stdout(Stdio::from(log_file));
    cmd.stderr(Stdio::from(log_file_dup));
    // Do NOT use kill_on_drop -- lifecycle managed explicitly

    let child = cmd.spawn().map_err(|e| {
        ProcessError::SpawnFailed(format!(
            "failed to spawn {}: {}",
            config.firecracker_bin.display(),
            e
        ))
    })?;

    let pid = child.id().ok_or_else(|| {
        ProcessError::SpawnFailed("child process exited before PID could be read".to_string())
    })?;

    // 5. Write PID file
    std::fs::write(&pid_file_path, pid.to_string()).map_err(|e| {
        ProcessError::PidFileError(format!(
            "failed to write PID file {}: {}",
            pid_file_path.display(),
            e
        ))
    })?;

    // 6. Create watch channel
    let (state_tx, state_rx) = watch::channel(ProcessState::Starting);

    // 7. Launch monitor task
    let monitor_handle = tokio::spawn(monitor_task(child, state_tx.clone(), pid));

    // 8. Return handle
    Ok(VmProcess {
        vm_id,
        child_pid: pid,
        state_rx,
        state_tx,
        monitor_handle,
        runtime_dir,
        api_socket_path,
        pid_file_path,
        log_file: Some(log_file_path),
        detached: false,
    })
}
