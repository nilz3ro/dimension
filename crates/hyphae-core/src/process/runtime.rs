//! Runtime directory management for Firecracker VM processes.
//!
//! Each VM gets a per-instance runtime directory under a base path resolved
//! from environment variables. Directory cleanup is synchronous for use in
//! `Drop` implementations.

use std::path::{Path, PathBuf};

use hyphae_errors::ProcessError;
use uuid::Uuid;

/// Resolve the base runtime directory for Hyphae VM state.
///
/// Resolution order:
/// 1. `HYPHAE_RUNTIME_DIR` env var (if set)
/// 2. `XDG_RUNTIME_DIR/hyphae` (if `XDG_RUNTIME_DIR` set)
/// 3. `/tmp/hyphae` (fallback)
///
/// This function only resolves the path -- it does not create directories.
pub fn runtime_base_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HYPHAE_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg).join("hyphae");
    }
    PathBuf::from("/tmp/hyphae")
}

/// Resolve the VMs subdirectory under the runtime base.
pub fn vms_dir(base: &Path) -> PathBuf {
    base.join("vms")
}

/// Resolve the deployments subdirectory under the runtime base.
///
/// Deployment VMs are placed here instead of `vms/` so that the orphan
/// reaper (which only scans `vms/`) never kills them.
pub fn deployments_dir(base: &Path) -> PathBuf {
    base.join("deployments")
}

/// Create a per-deployment runtime directory.
///
/// Creates `$BASE/deployments/{uuid_simple}/` and returns the path.
/// The UUID is formatted without hyphens for filesystem friendliness.
///
/// CRITICAL: Uses `deployments_dir()`, NOT `vms_dir()`. This means the
/// orphan reaper will never scan or kill processes in this directory.
pub fn create_deployment_runtime_dir(base: &Path, deployment_id: &Uuid) -> Result<PathBuf, ProcessError> {
    let dir = deployments_dir(base).join(deployment_id.simple().to_string());
    std::fs::create_dir_all(&dir).map_err(|e| {
        ProcessError::RuntimeDirError(format!(
            "failed to create deployment runtime directory {}: {}",
            dir.display(),
            e
        ))
    })?;
    tracing::trace!(path = %dir.display(), "created deployment runtime directory");
    Ok(dir)
}

/// Create a per-VM runtime directory.
///
/// Creates `$BASE/vms/{uuid_simple}/` and returns the path.
/// The UUID is formatted without hyphens for filesystem friendliness.
pub fn create_vm_runtime_dir(base: &Path, vm_id: &Uuid) -> Result<PathBuf, ProcessError> {
    let dir = vms_dir(base).join(vm_id.simple().to_string());
    std::fs::create_dir_all(&dir).map_err(|e| {
        ProcessError::RuntimeDirError(format!(
            "failed to create VM runtime directory {}: {}",
            dir.display(),
            e
        ))
    })?;
    tracing::trace!(path = %dir.display(), "created VM runtime directory");
    Ok(dir)
}

/// Synchronous cleanup of a VM runtime directory.
///
/// Removes the socket file, PID file, and the directory itself.
/// Each removal is logged at trace level. Errors are logged but not
/// propagated because cleanup must be best-effort (used in `Drop`).
pub fn cleanup_runtime_dir_sync(dir: &Path) {
    let socket = dir.join("firecracker.sock");
    let pid_file = dir.join("firecracker.pid");

    if socket.exists() {
        match std::fs::remove_file(&socket) {
            Ok(()) => tracing::trace!(path = %socket.display(), "removed socket file"),
            Err(e) => tracing::trace!(path = %socket.display(), error = %e, "failed to remove socket"),
        }
    }

    if pid_file.exists() {
        match std::fs::remove_file(&pid_file) {
            Ok(()) => tracing::trace!(path = %pid_file.display(), "removed PID file"),
            Err(e) => tracing::trace!(path = %pid_file.display(), error = %e, "failed to remove PID file"),
        }
    }

    match std::fs::remove_dir_all(dir) {
        Ok(()) => tracing::trace!(path = %dir.display(), "removed runtime directory"),
        Err(e) => tracing::trace!(path = %dir.display(), error = %e, "failed to remove runtime dir"),
    }
}
