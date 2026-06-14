//! Integration tests for process spawning, monitoring, and state observation.
//!
//! Uses `/bin/sh -c` as a mock "firecracker" binary -- SpawnConfig.firecracker_bin
//! accepts any binary path, so tests don't need an actual firecracker.

use std::time::Duration;

use hyphae_core::process::{spawn, ProcessState, SpawnConfig};
use hyphae_errors::ProcessError;

/// Create a shell script wrapper in a temp directory that ignores firecracker
/// flags (--api-sock, etc.) and runs the given shell command body.
fn write_test_script(dir: &std::path::Path, script_body: &str) -> std::path::PathBuf {
    let script_path = dir.join("mock-firecracker.sh");
    std::fs::write(
        &script_path,
        format!("#!/bin/sh\n{}\n", script_body),
    )
    .expect("failed to write test script");

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("failed to chmod test script");
    }

    // Ensure the file is fully flushed to disk before exec — avoids ETXTBSY
    // ("Text file busy") race when the OS hasn't finished writing the script.
    {
        let f = std::fs::File::open(&script_path).expect("failed to open for sync");
        f.sync_all().expect("failed to sync test script");
        drop(f);
        // Brief yield to let the kernel release the write reference
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    script_path
}

#[tokio::test]
async fn test_spawn_creates_runtime_dir() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let script = write_test_script(tmp.path(), "sleep 5");

    let config = SpawnConfig {
        firecracker_bin: script.clone(),
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let process = spawn(&config).await.expect("spawn failed");

    // Runtime directory should exist
    assert!(process.runtime_dir().exists(), "runtime dir should exist");

    // PID file should exist and contain the PID
    let pid_file = process.runtime_dir().join("firecracker.pid");
    assert!(pid_file.exists(), "PID file should exist");

    let pid_contents = std::fs::read_to_string(&pid_file).expect("failed to read PID file");
    let file_pid: u32 = pid_contents.trim().parse().expect("PID file should contain a number");
    assert_eq!(file_pid, process.pid(), "PID file contents should match process PID");

    // Clean up: kill the process so it doesn't linger
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(process.pid() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

#[tokio::test]
async fn test_spawn_returns_correct_pid() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let script = write_test_script(tmp.path(), "sleep 5");

    let config = SpawnConfig {
        firecracker_bin: script.clone(),
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let process = spawn(&config).await.expect("spawn failed");

    // PID should be > 0
    assert!(process.pid() > 0, "PID should be positive");

    // PID file should match
    let pid_file = process.runtime_dir().join("firecracker.pid");
    let file_pid: u32 = std::fs::read_to_string(&pid_file)
        .expect("failed to read PID file")
        .trim()
        .parse()
        .expect("PID should be a number");
    assert_eq!(process.pid(), file_pid);

    // Clean up
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(process.pid() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

#[tokio::test]
async fn test_monitor_reaps_process() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    // Process that exits immediately with code 0
    let script = write_test_script(tmp.path(), "exit 0");

    let config = SpawnConfig {
        firecracker_bin: script.clone(),
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.expect("spawn failed");

    // Wait for the process to exit
    let exit_info = process.wait().await;

    // Should have exit code 0
    assert_eq!(exit_info.exit_code, Some(0), "exit code should be 0");
    assert_eq!(exit_info.signal, None, "signal should be None for clean exit");

    // State should be Exited
    assert!(
        matches!(process.state(), ProcessState::Exited(_)),
        "state should be Exited after wait"
    );
}

#[tokio::test]
async fn test_state_transitions() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    // Process that runs briefly then exits
    let script = write_test_script(tmp.path(), "sleep 0.1; exit 0");

    let config = SpawnConfig {
        firecracker_bin: script.clone(),
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.expect("spawn failed");

    // Subscribe before waiting so we can track transitions
    let mut rx = process.subscribe();

    // Wait for process to exit
    let _info = process.wait().await;

    // Final state should be Exited
    let final_state = rx.borrow_and_update().clone();
    assert!(
        matches!(final_state, ProcessState::Exited(_)),
        "final state should be Exited, got {:?}",
        final_state
    );
}

#[tokio::test]
async fn test_is_running() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let script = write_test_script(tmp.path(), "sleep 5");

    let config = SpawnConfig {
        firecracker_bin: script.clone(),
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.expect("spawn failed");

    // Give monitor task a moment to transition to Running
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Should be running
    assert!(process.is_running(), "process should be running");

    // Kill the process
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(process.pid() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }

    // Wait for exit
    let _info = process.wait().await;

    // Should no longer be running
    assert!(!process.is_running(), "process should not be running after exit");
}

#[tokio::test]
async fn test_wait_returns_exit_info() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    // Process that exits with code 42
    let script = write_test_script(tmp.path(), "exit 42");

    let config = SpawnConfig {
        firecracker_bin: script.clone(),
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.expect("spawn failed");

    let exit_info = process.wait().await;
    assert_eq!(exit_info.exit_code, Some(42), "exit code should be 42");
}

#[tokio::test]
async fn test_subscribe_receives_state() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let script = write_test_script(tmp.path(), "exit 0");

    let config = SpawnConfig {
        firecracker_bin: script.clone(),
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.expect("spawn failed");

    // Create a second observer
    let mut rx2 = process.subscribe();

    // Wait on the primary handle
    let _info = process.wait().await;

    // The subscriber should also see the Exited state
    // Wait for the subscriber to receive the update
    let _ = rx2.changed().await;
    let state = rx2.borrow().clone();
    assert!(
        matches!(state, ProcessState::Exited(_)),
        "subscriber should see Exited state, got {:?}",
        state
    );
}

#[tokio::test]
async fn test_wait_ready_timeout() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    // Process that does NOT create a socket -- just sleeps
    let script = write_test_script(tmp.path(), "sleep 5");

    let config = SpawnConfig {
        firecracker_bin: script.clone(),
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let process = spawn(&config).await.expect("spawn failed");

    // wait_ready with a very short timeout should fail
    let result = process.wait_ready(Duration::from_millis(100)).await;
    assert!(result.is_err(), "wait_ready should timeout");

    match result.unwrap_err() {
        ProcessError::WaitReadyTimeout => {} // expected
        other => panic!("expected WaitReadyTimeout, got {:?}", other),
    }

    // Clean up
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(process.pid() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}
