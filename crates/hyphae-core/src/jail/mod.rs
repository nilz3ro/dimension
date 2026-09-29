//! Jailer subsystem: system user validation, cgroup v2 resource limits,
//! jail directory creation, resource hard-linking, and JailConfig command builder.
//!
//! The Firecracker jailer provides chroot-based isolation with cgroup
//! resource limits and privilege dropping. This module contains the
//! primitives for configuring and invoking the jailer binary.

pub mod cgroup;
pub mod setup;
pub mod user;

pub use cgroup::{CgroupConfig, create_cgroup, remove_cgroup};
pub use setup::{
    cleanup_jail, create_jail_directory, jail_root_dir, stage_resources_into_jail,
};
pub use user::{JailUser, validate_jail_user};

use std::path::{Path, PathBuf};
use std::time::Duration;

use hyphae_errors::JailError;
use tracing::{debug, trace};

/// Default chroot base directory for jailed VMs.
pub const DEFAULT_CHROOT_BASE: &str = "/srv/hyphae/jails";

/// Default timeout for waiting on the Firecracker PID file.
const PID_FILE_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll interval when waiting for the PID file.
const PID_FILE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Configuration for invoking the Firecracker jailer.
///
/// Drives the jailer CLI invocation with all required arguments:
/// `--id`, `--exec-file`, `--uid`, `--gid`, `--chroot-base-dir`,
/// `--cgroup-version`, optional `--daemonize`, and pass-through
/// Firecracker args after `--`.
pub struct JailConfig {
    /// Path to the jailer binary.
    pub jailer_bin: PathBuf,
    /// Path to the Firecracker binary (passed as `--exec-file`).
    pub firecracker_bin: PathBuf,
    /// Unique VM identifier.
    pub vm_id: String,
    /// UID to drop privileges to.
    pub uid: u32,
    /// GID to drop privileges to.
    pub gid: u32,
    /// Base directory for chroot jails.
    pub chroot_base_dir: PathBuf,
    /// Cgroup version (always 2).
    pub cgroup_version: u8,
    /// Whether to daemonize the jailer process.
    pub daemonize: bool,
}

impl JailConfig {
    /// Create a new JailConfig with defaults.
    ///
    /// Defaults:
    /// - `chroot_base_dir`: `/srv/hyphae/jails`
    /// - `cgroup_version`: 2
    /// - `daemonize`: true
    pub fn new(
        jailer_bin: PathBuf,
        firecracker_bin: PathBuf,
        vm_id: String,
        uid: u32,
        gid: u32,
    ) -> Self {
        Self {
            jailer_bin,
            firecracker_bin,
            vm_id,
            uid,
            gid,
            chroot_base_dir: PathBuf::from(DEFAULT_CHROOT_BASE),
            cgroup_version: 2,
            daemonize: true,
        }
    }

    /// Build a `tokio::process::Command` for the jailer invocation.
    ///
    /// Arguments passed to the jailer:
    /// - `--id {vm_id}`
    /// - `--exec-file {firecracker_bin}`
    /// - `--uid {uid}`
    /// - `--gid {gid}`
    /// - `--chroot-base-dir {chroot_base_dir}`
    /// - `--cgroup-version {cgroup_version}`
    /// - `--daemonize` (if enabled)
    /// - `-- --api-sock {api_sock}` (pass-through Firecracker args)
    pub fn build_command(&self, api_sock_path: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.jailer_bin);

        cmd.arg("--id")
            .arg(&self.vm_id)
            .arg("--exec-file")
            .arg(&self.firecracker_bin)
            .arg("--uid")
            .arg(self.uid.to_string())
            .arg("--gid")
            .arg(self.gid.to_string())
            .arg("--chroot-base-dir")
            .arg(&self.chroot_base_dir)
            .arg("--cgroup-version")
            .arg(self.cgroup_version.to_string());

        if self.daemonize {
            cmd.arg("--daemonize");
        }

        // Pass-through Firecracker arguments after --.
        cmd.arg("--")
            .arg("--api-sock")
            .arg(api_sock_path);

        debug!(
            jailer = %self.jailer_bin.display(),
            vm_id = %self.vm_id,
            uid = self.uid,
            gid = self.gid,
            "built jailer command"
        );

        cmd
    }

    /// Get the jail root directory path for this configuration.
    pub fn jail_root(&self) -> PathBuf {
        jail_root_dir(&self.chroot_base_dir, &self.firecracker_bin, &self.vm_id)
    }

    /// Clean up the jail directory for this VM.
    pub fn cleanup(&self) -> Result<(), JailError> {
        cleanup_jail(&self.chroot_base_dir, &self.firecracker_bin, &self.vm_id)
    }
}

/// Discover the jailer binary.
///
/// Strategy:
/// 1. If a Firecracker binary path is provided, look for `jailer` as a sibling.
/// 2. Fall back to searching PATH via `which jailer`.
pub fn find_jailer(firecracker_bin: Option<&Path>) -> Result<PathBuf, JailError> {
    // Strategy 1: Look for jailer as a sibling of the Firecracker binary.
    if let Some(fc_path) = firecracker_bin {
        if let Some(parent) = fc_path.parent() {
            let sibling = parent.join("jailer");
            if sibling.exists() {
                debug!(path = %sibling.display(), "found jailer as sibling of firecracker");
                return Ok(sibling);
            }
        }
    }

    // Strategy 2: Search PATH via `which`.
    let output = std::process::Command::new("which")
        .arg("jailer")
        .output()
        .map_err(|_e| {
            JailError::JailerNotFound {
                path: PathBuf::from("jailer"),
            }
        })?;

    if output.status.success() {
        let path_str = String::from_utf8_lossy(&output.stdout);
        let path = PathBuf::from(path_str.trim());
        if path.exists() {
            debug!(path = %path.display(), "found jailer in PATH");
            return Ok(path);
        }
    }

    Err(JailError::JailerNotFound {
        path: PathBuf::from("jailer"),
    })
}

/// Wait for Firecracker to write its PID file inside the jail.
///
/// The jailer starts Firecracker in the background when `--daemonize` is used.
/// Firecracker writes its PID to a well-known location. This function polls
/// for that file with a timeout.
pub async fn wait_for_pid_file(pid_file_path: &Path) -> Result<u32, JailError> {
    wait_for_pid_file_with_timeout(pid_file_path, PID_FILE_TIMEOUT).await
}

/// Wait for Firecracker PID file with a configurable timeout.
pub async fn wait_for_pid_file_with_timeout(
    pid_file_path: &Path,
    timeout: Duration,
) -> Result<u32, JailError> {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        if pid_file_path.exists() {
            let content = std::fs::read_to_string(pid_file_path).map_err(|e| {
                JailError::InvalidPidFile {
                    path: pid_file_path.to_path_buf(),
                    content: e.to_string(),
                }
            })?;

            let pid: u32 = content.trim().parse().map_err(|_| JailError::InvalidPidFile {
                path: pid_file_path.to_path_buf(),
                content: content.trim().to_string(),
            })?;

            trace!(pid, path = %pid_file_path.display(), "read PID from file");
            return Ok(pid);
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(JailError::PidFileTimeout {
                path: pid_file_path.to_path_buf(),
            });
        }

        tokio::time::sleep(PID_FILE_POLL_INTERVAL).await;
    }
}
