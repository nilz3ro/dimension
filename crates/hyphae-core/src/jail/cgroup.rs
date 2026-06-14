//! Cgroup v2 creation and teardown via filesystem writes.
//!
//! Creates per-VM cgroup directories under `/sys/fs/cgroup/hyphae/` and
//! configures CPU and memory resource limits.

use std::path::{Path, PathBuf};

use hyphae_errors::JailError;
use tracing::{info, trace};

/// Configuration for cgroup resource limits.
pub struct CgroupConfig {
    /// Number of vCPUs allocated to the VM.
    pub vcpus: u8,
    /// Memory allocation in MiB.
    pub memory_mib: u64,
    /// Unique VM identifier for the cgroup directory name.
    pub vm_id: String,
}

const CGROUP_BASE: &str = "/sys/fs/cgroup";

/// Create a cgroup v2 directory for a VM with CPU and memory limits.
///
/// Creates `/sys/fs/cgroup/hyphae/{vm_id}/` and writes:
/// - `cpu.max`: `{vcpus * 100000} 100000`
/// - `memory.max`: `{memory_mib * 1024 * 1024}` bytes
///
/// The parent `/sys/fs/cgroup/hyphae/` directory is created on first use
/// with `+cpu +memory` enabled in `cgroup.subtree_control`.
pub fn create_cgroup(config: &CgroupConfig) -> Result<PathBuf, JailError> {
    let parent = Path::new(CGROUP_BASE).join("hyphae");
    let cgroup_dir = parent.join(&config.vm_id);

    // Create parent hyphae cgroup if it doesn't exist.
    if !parent.exists() {
        std::fs::create_dir(&parent).map_err(|e| {
            JailError::CgroupSetupFailed(format!(
                "failed to create parent cgroup {}: {e}",
                parent.display()
            ))
        })?;

        // Enable CPU and memory controllers in the parent.
        let subtree_control = parent.join("cgroup.subtree_control");
        std::fs::write(&subtree_control, "+cpu +memory").map_err(|e| {
            JailError::CgroupSetupFailed(format!(
                "failed to write subtree_control at {}: {e}",
                subtree_control.display()
            ))
        })?;
    }

    // Create VM-specific cgroup directory.
    std::fs::create_dir(&cgroup_dir).map_err(|e| {
        JailError::CgroupSetupFailed(format!(
            "failed to create cgroup {}: {e}",
            cgroup_dir.display()
        ))
    })?;

    // Write CPU limit: max = vcpus * period, period = 100000.
    let period: u64 = 100_000;
    let max = u64::from(config.vcpus) * period;
    let cpu_max_path = cgroup_dir.join("cpu.max");
    std::fs::write(&cpu_max_path, format!("{max} {period}")).map_err(|e| {
        JailError::CgroupSetupFailed(format!(
            "failed to write cpu.max at {}: {e}",
            cpu_max_path.display()
        ))
    })?;

    // Write memory limit in bytes.
    let memory_bytes = config.memory_mib * 1024 * 1024;
    let memory_max_path = cgroup_dir.join("memory.max");
    std::fs::write(&memory_max_path, memory_bytes.to_string()).map_err(|e| {
        JailError::CgroupSetupFailed(format!(
            "failed to write memory.max at {}: {e}",
            memory_max_path.display()
        ))
    })?;

    info!(
        cgroup = %cgroup_dir.display(),
        vcpus = config.vcpus,
        memory_mib = config.memory_mib,
        "created cgroup with resource limits"
    );

    Ok(cgroup_dir)
}

/// Remove the cgroup directory for a VM.
///
/// Uses `std::fs::remove_dir()` (not `remove_dir_all`) because the kernel
/// manages cgroup file cleanup -- only the directory itself needs removal.
///
/// Returns `Ok(())` silently if the directory does not exist.
pub fn remove_cgroup(vm_id: &str) -> Result<(), JailError> {
    let cgroup_dir = Path::new(CGROUP_BASE).join("hyphae").join(vm_id);

    if !cgroup_dir.exists() {
        return Ok(());
    }

    std::fs::remove_dir(&cgroup_dir).map_err(|e| {
        JailError::CgroupSetupFailed(format!(
            "failed to remove cgroup {}: {e}",
            cgroup_dir.display()
        ))
    })?;

    trace!(cgroup = %cgroup_dir.display(), "removed cgroup directory");

    Ok(())
}
