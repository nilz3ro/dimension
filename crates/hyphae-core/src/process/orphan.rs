//! Orphan recovery and stale resource cleanup for Firecracker VMs.
//!
//! Scans runtime directories for leftover VM state from previous crashes,
//! verifies stale processes, kills confirmed orphans, and cleans up all
//! resources.

use std::path::Path;

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

use crate::process::runtime::{cleanup_runtime_dir_sync, vms_dir};

/// Report from orphan recovery.
#[derive(Debug, Default, Clone)]
pub struct RecoveryReport {
    /// Number of confirmed Firecracker processes killed.
    pub killed: usize,
    /// Number of stale PID files (process dead or reused by non-firecracker).
    pub stale_pids: usize,
    /// Number of directories without PID files cleaned up.
    pub stale_dirs: usize,
    /// Total directories cleaned up.
    pub cleaned: usize,
    /// Number of errors encountered (non-fatal).
    pub errors: usize,
}

/// Check whether a process with the given PID is a running Firecracker instance.
///
/// On Linux, reads `/proc/{pid}/exe` to check if the executable filename
/// is `firecracker`. Returns `false` on any error (process gone, permission
/// denied, etc.).
///
/// On non-Linux platforms (e.g., macOS for development/testing), always
/// returns `false` since `/proc` does not exist.
#[cfg(target_os = "linux")]
fn is_firecracker_process(pid: i32) -> bool {
    let exe_path = format!("/proc/{pid}/exe");
    match std::fs::read_link(&exe_path) {
        Ok(target) => {
            let is_fc = target
                .file_name()
                .is_some_and(|name| name == "firecracker");
            tracing::trace!(pid, path = %target.display(), is_firecracker = is_fc, "checked process exe");
            is_fc
        }
        Err(e) => {
            tracing::trace!(pid, error = %e, "could not read /proc/{pid}/exe");
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn is_firecracker_process(pid: i32) -> bool {
    tracing::trace!(pid, "is_firecracker_process: not on Linux, returning false");
    false
}

/// Check whether a process with the given PID is still alive.
///
/// Uses `kill(pid, 0)` which checks if the process exists without sending
/// a signal.
fn is_process_alive(pid: i32) -> bool {
    signal::kill(Pid::from_raw(pid), None).is_ok()
}

/// Scan for orphaned VM runtime directories and clean them up.
///
/// Iterates through VM subdirectories under `runtime_base/vms/`, checks for
/// stale PID files, verifies whether referenced processes are still running
/// Firecracker, kills confirmed orphans, and removes all stale resources.
///
/// This function is designed to run as a background task via `tokio::spawn`.
/// All errors are non-fatal -- they are logged and counted in the report.
pub async fn recover_orphans(runtime_base: &Path) -> RecoveryReport {
    let mut report = RecoveryReport::default();

    let vms = vms_dir(runtime_base);
    let entries = match std::fs::read_dir(&vms) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::trace!(path = %vms.display(), error = %e, "vms directory not readable, returning empty report");
            return report;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::trace!(error = %e, "failed to read directory entry");
                report.errors += 1;
                continue;
            }
        };

        let vm_dir = entry.path();

        // Only process directories.
        if !vm_dir.is_dir() {
            continue;
        }

        let pid_file = vm_dir.join("firecracker.pid");

        if !pid_file.exists() {
            // No PID file -- stale directory, clean it up.
            tracing::trace!(path = %vm_dir.display(), "stale VM directory (no PID file), cleaning up");
            cleanup_runtime_dir_sync(&vm_dir);
            report.stale_dirs += 1;
            report.cleaned += 1;
            continue;
        }

        // Read and parse PID file.
        let pid_contents = match std::fs::read_to_string(&pid_file) {
            Ok(contents) => contents,
            Err(e) => {
                tracing::trace!(path = %pid_file.display(), error = %e, "failed to read PID file, cleaning up");
                cleanup_runtime_dir_sync(&vm_dir);
                report.stale_pids += 1;
                report.cleaned += 1;
                continue;
            }
        };

        let pid: i32 = match pid_contents.trim().parse() {
            Ok(p) => p,
            Err(e) => {
                tracing::trace!(
                    path = %pid_file.display(),
                    contents = %pid_contents.trim(),
                    error = %e,
                    "malformed PID file, cleaning up"
                );
                cleanup_runtime_dir_sync(&vm_dir);
                report.stale_pids += 1;
                report.cleaned += 1;
                continue;
            }
        };

        // Check if the process is still alive.
        if !is_process_alive(pid) {
            tracing::trace!(pid, path = %vm_dir.display(), "process is dead, cleaning up stale directory");
            cleanup_runtime_dir_sync(&vm_dir);
            report.stale_pids += 1;
            report.cleaned += 1;
            continue;
        }

        // Process is alive -- check if it's actually Firecracker.
        if is_firecracker_process(pid) {
            tracing::trace!(pid, path = %vm_dir.display(), "confirmed orphaned Firecracker process, killing");
            match signal::kill(Pid::from_raw(pid), Signal::SIGKILL) {
                Ok(()) => {
                    tracing::trace!(pid, "sent SIGKILL to orphaned Firecracker");
                    report.killed += 1;
                }
                Err(e) => {
                    tracing::trace!(pid, error = %e, "failed to kill orphaned Firecracker");
                    report.errors += 1;
                }
            }
            cleanup_runtime_dir_sync(&vm_dir);
            report.cleaned += 1;
        } else {
            // PID reused by a non-Firecracker process -- clean up files only.
            tracing::trace!(
                pid,
                path = %vm_dir.display(),
                "PID reused by non-Firecracker process, cleaning stale files only"
            );
            cleanup_runtime_dir_sync(&vm_dir);
            report.stale_pids += 1;
            report.cleaned += 1;
        }
    }

    tracing::trace!(
        killed = report.killed,
        stale_pids = report.stale_pids,
        stale_dirs = report.stale_dirs,
        cleaned = report.cleaned,
        errors = report.errors,
        "orphan recovery complete"
    );

    report
}
