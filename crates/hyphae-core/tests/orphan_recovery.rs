//! Integration tests for orphan recovery and stale resource cleanup.

use std::fs;
use std::path::Path;

use hyphae_core::process::recover_orphans;
use tempfile::tempdir;

/// Helper: create a VM subdirectory under `base/vms/{name}/`.
fn create_vm_dir(base: &Path, name: &str) -> std::path::PathBuf {
    let dir = base.join("vms").join(name);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Helper: write a PID file in a VM directory.
fn write_pid_file(vm_dir: &Path, contents: &str) {
    fs::write(vm_dir.join("firecracker.pid"), contents).unwrap();
}

/// Helper: create a fake socket file in a VM directory.
fn create_socket_file(vm_dir: &Path) {
    fs::write(vm_dir.join("firecracker.sock"), "").unwrap();
}

#[tokio::test]
async fn test_empty_runtime_dir() {
    let tmp = tempdir().unwrap();
    // Create vms/ but leave it empty.
    fs::create_dir_all(tmp.path().join("vms")).unwrap();

    let report = recover_orphans(tmp.path()).await;

    assert_eq!(report.killed, 0);
    assert_eq!(report.stale_pids, 0);
    assert_eq!(report.stale_dirs, 0);
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn test_stale_dir_no_pid_file() {
    let tmp = tempdir().unwrap();
    let vm_dir = create_vm_dir(tmp.path(), "abc123");

    // No PID file -- just the directory.
    assert!(vm_dir.exists());

    let report = recover_orphans(tmp.path()).await;

    assert_eq!(report.stale_dirs, 1);
    assert_eq!(report.cleaned, 1);
    // Directory should be removed.
    assert!(!vm_dir.exists(), "stale VM directory should be cleaned up");
}

#[tokio::test]
async fn test_stale_pid_dead_process() {
    let tmp = tempdir().unwrap();
    let vm_dir = create_vm_dir(tmp.path(), "dead_pid_vm");

    // PID 999999999 almost certainly doesn't exist.
    write_pid_file(&vm_dir, "999999999");

    let report = recover_orphans(tmp.path()).await;

    assert_eq!(report.stale_pids, 1);
    assert_eq!(report.cleaned, 1);
    assert!(!vm_dir.exists(), "VM directory with dead PID should be cleaned up");
}

#[tokio::test]
async fn test_malformed_pid_file() {
    let tmp = tempdir().unwrap();
    let vm_dir = create_vm_dir(tmp.path(), "malformed_vm");

    write_pid_file(&vm_dir, "not_a_number");

    let report = recover_orphans(tmp.path()).await;

    // Malformed PID is treated as stale.
    assert_eq!(report.stale_pids, 1);
    assert_eq!(report.cleaned, 1);
    assert!(!vm_dir.exists(), "VM directory with malformed PID should be cleaned up");
}

#[tokio::test]
async fn test_pid_reused_not_firecracker() {
    let tmp = tempdir().unwrap();
    let vm_dir = create_vm_dir(tmp.path(), "reused_pid_vm");

    // Use our own PID -- it's alive but definitely not Firecracker.
    let my_pid = std::process::id();
    write_pid_file(&vm_dir, &my_pid.to_string());

    let report = recover_orphans(tmp.path()).await;

    // Should NOT kill us. The PID is alive but not Firecracker.
    assert_eq!(report.killed, 0, "must NOT kill non-firecracker process");
    assert_eq!(report.stale_pids, 1);
    assert_eq!(report.cleaned, 1);
    assert!(!vm_dir.exists(), "VM directory should still be cleaned up");

    // Verify we're still alive (trivially true if we reach this point).
}

#[tokio::test]
async fn test_mixed_recovery() {
    let tmp = tempdir().unwrap();

    // VM 1: stale dir without PID file.
    create_vm_dir(tmp.path(), "stale_no_pid");

    // VM 2: dead PID.
    let vm2 = create_vm_dir(tmp.path(), "dead_pid");
    write_pid_file(&vm2, "999999998");

    // VM 3: malformed PID.
    let vm3 = create_vm_dir(tmp.path(), "malformed");
    write_pid_file(&vm3, "garbage");

    // VM 4: PID reused by non-firecracker (our process).
    let vm4 = create_vm_dir(tmp.path(), "reused");
    write_pid_file(&vm4, &std::process::id().to_string());

    let report = recover_orphans(tmp.path()).await;

    assert_eq!(report.stale_dirs, 1, "one dir without PID file");
    assert_eq!(report.stale_pids, 3, "dead + malformed + reused = 3 stale PIDs");
    assert_eq!(report.cleaned, 4, "all four dirs cleaned");
    assert_eq!(report.killed, 0, "no firecracker processes to kill");
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn test_nonexistent_runtime_base() {
    let tmp = tempdir().unwrap();
    let nonexistent = tmp.path().join("does_not_exist");

    let report = recover_orphans(&nonexistent).await;

    // Should return empty report without panicking.
    assert_eq!(report.killed, 0);
    assert_eq!(report.stale_pids, 0);
    assert_eq!(report.stale_dirs, 0);
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn test_stale_socket_file_cleaned() {
    let tmp = tempdir().unwrap();
    let vm_dir = create_vm_dir(tmp.path(), "socket_vm");

    write_pid_file(&vm_dir, "999999997");
    create_socket_file(&vm_dir);

    // Verify files exist before recovery.
    assert!(vm_dir.join("firecracker.sock").exists());
    assert!(vm_dir.join("firecracker.pid").exists());

    let report = recover_orphans(tmp.path()).await;

    assert_eq!(report.stale_pids, 1);
    assert_eq!(report.cleaned, 1);
    // Both the socket file and the directory should be gone.
    assert!(!vm_dir.join("firecracker.sock").exists(), "socket file should be cleaned");
    assert!(!vm_dir.exists(), "VM directory should be cleaned up");
}
