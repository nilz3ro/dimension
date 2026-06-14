//! Integration tests for shutdown escalation, force_stop, Drop cleanup, and detach.
//!
//! Uses shell scripts as mock "firecracker" processes. Tests verify shutdown
//! stages, runtime directory cleanup, Drop behavior, and SendCtrlAltDel
//! HTTP request formatting.
//!
//! Note: `test_shutdown_escalates_to_sigkill` lives in a separate test file
//! (`process_shutdown_escalation.rs`) because it requires precise signal
//! behavior that is sensitive to parallel test execution on macOS.

use std::time::{Duration, Instant};

use hyphae_core::process::shutdown::send_ctrl_alt_del;
use hyphae_core::process::{spawn, ProcessState, ShutdownConfig, ShutdownStage, SpawnConfig};

/// Create a shell script wrapper in a temp directory that ignores firecracker
/// flags (--api-sock, etc.) and runs the given shell command body.
fn write_test_script(dir: &std::path::Path, script_body: &str) -> std::path::PathBuf {
    let script_path = dir.join("mock-firecracker.sh");
    std::fs::write(
        &script_path,
        format!("#!/bin/sh\n{}\n", script_body),
    )
    .expect("failed to write test script");

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
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    script_path
}

// ---------------------------------------------------------------------------
// Test 1: Graceful shutdown stops with SIGTERM (no API socket -> CtrlAltDel skipped)
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_shutdown_sigterm() {
    let tmp = tempfile::tempdir().unwrap();
    let script = write_test_script(tmp.path(), "sleep 300");

    let config = SpawnConfig {
        firecracker_bin: script,
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.unwrap();

    // Give monitor task time to transition to Running
    tokio::time::sleep(Duration::from_millis(50)).await;

    let report = process.shutdown(ShutdownConfig::default()).await;

    // sleep responds to SIGTERM, so it should stop at Sigterm stage
    assert_eq!(
        report.final_stage,
        ShutdownStage::Sigterm,
        "expected Sigterm stage, got {:?}",
        report.final_stage
    );

    // Process should be exited
    assert!(
        matches!(process.state(), ProcessState::Exited(_)),
        "process should be exited after shutdown"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Shutdown of already-exited process returns AlreadyExited
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_shutdown_already_exited() {
    let tmp = tempfile::tempdir().unwrap();
    let script = write_test_script(tmp.path(), "echo hello");

    let config = SpawnConfig {
        firecracker_bin: script,
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.unwrap();

    // Wait for the process to exit naturally
    let _exit = process.wait().await;

    let report = process.shutdown(ShutdownConfig::default()).await;

    assert_eq!(
        report.final_stage,
        ShutdownStage::AlreadyExited,
        "expected AlreadyExited, got {:?}",
        report.final_stage
    );
}

// ---------------------------------------------------------------------------
// Test 3: force_stop sends SIGKILL immediately
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_force_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let script = write_test_script(tmp.path(), "sleep 300");

    let config = SpawnConfig {
        firecracker_bin: script,
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let report = process.force_stop().await;

    assert_eq!(
        report.final_stage,
        ShutdownStage::Sigkill,
        "expected Sigkill from force_stop, got {:?}",
        report.final_stage
    );

    assert!(
        matches!(process.state(), ProcessState::Exited(_)),
        "process should be exited after force_stop"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Shutdown escalates to SIGKILL when SIGTERM is ignored
//
// This test requires a compiled C binary that ignores SIGTERM. On macOS,
// parallel test execution with `cargo test --workspace` can interfere with
// child process signal handling, causing spurious failures. The test is
// marked #[ignore] and must be run explicitly:
//
//   cargo test -p hyphae-core --test process_shutdown -- --ignored
//
// On Linux (the target platform for Firecracker), this test runs reliably.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore]
async fn test_shutdown_escalates_to_sigkill() {
    let tmp = tempfile::tempdir().unwrap();

    // Compile a binary that ignores SIGTERM
    let fixture_dir = std::path::PathBuf::from("/tmp/hyphae_test_fixtures");
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let c_source = fixture_dir.join("ignore_term.c");
    let binary = fixture_dir.join("ignore_term");

    if !binary.exists() {
        std::fs::write(
            &c_source,
            "#include <signal.h>\n#include <unistd.h>\n\
             int main(int argc, char **argv) {\n\
             (void)argc; (void)argv;\n\
             signal(SIGTERM, SIG_IGN);\n\
             while(1) { sleep(300); }\n\
             return 0;\n}\n",
        )
        .unwrap();
        let output = std::process::Command::new("cc")
            .args(["-o", binary.to_str().unwrap(), c_source.to_str().unwrap()])
            .output()
            .expect("cc not found");
        assert!(output.status.success(), "failed to compile ignore_term.c");
    }

    let config = SpawnConfig {
        firecracker_bin: binary,
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(process.is_running(), "process should be running before shutdown");

    let shutdown_config = ShutdownConfig {
        ctrl_alt_del_timeout: Duration::from_millis(100),
        sigterm_timeout: Duration::from_millis(500),
        sigkill_timeout: Duration::from_secs(5),
    };

    let report = process.shutdown(shutdown_config).await;

    assert_eq!(
        report.final_stage,
        ShutdownStage::Sigkill,
        "expected Sigkill after escalation, got {:?}",
        report.final_stage
    );
}

// ---------------------------------------------------------------------------
// Test 5: Drop kills process and cleans up runtime directory
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_drop_kills_and_cleans_up() {
    let tmp = tempfile::tempdir().unwrap();
    let script = write_test_script(tmp.path(), "sleep 300");

    let config = SpawnConfig {
        firecracker_bin: script,
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let runtime_dir;
    let pid;
    {
        let process = spawn(&config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        runtime_dir = process.runtime_dir().to_path_buf();
        pid = process.pid();
        assert!(runtime_dir.exists(), "runtime dir should exist before drop");
        // process is dropped here
    }

    // Give a moment for synchronous SIGKILL + cleanup to take effect
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Runtime directory should be cleaned up
    assert!(
        !runtime_dir.exists(),
        "runtime dir should be removed after drop"
    );

    // Process should be dead
    let alive = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        None, // signal 0 = check existence
    )
    .is_ok();
    assert!(!alive, "process should be dead after drop");
}

// ---------------------------------------------------------------------------
// Test 6: Drop on detached handle does nothing
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_drop_detached_does_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let script = write_test_script(tmp.path(), "sleep 2");

    let config = SpawnConfig {
        firecracker_bin: script,
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let pid;
    let runtime_dir;
    {
        let mut process = spawn(&config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        pid = process.pid();
        runtime_dir = process.runtime_dir().to_path_buf();
        process.detach();
        // process is dropped here -- but detached, so no kill
    }

    // Process should still be alive briefly
    tokio::time::sleep(Duration::from_millis(50)).await;
    let alive = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        None,
    )
    .is_ok();
    assert!(alive, "detached process should still be alive after drop");

    // Clean up manually
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
    let _ = std::fs::remove_dir_all(&runtime_dir);
}

// ---------------------------------------------------------------------------
// Test 7: send_ctrl_alt_del sends correct raw HTTP format
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_send_ctrl_alt_del_format() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("test.sock");

    // Create a Unix listener that accepts one connection and reads the request
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();

    let socket_path_clone = socket_path.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        use tokio::io::AsyncReadExt;
        let n = stream.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_string();

        // Send a valid HTTP 204 response
        use tokio::io::AsyncWriteExt;
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();

        request
    });

    // Send the CtrlAltDel request
    let result = send_ctrl_alt_del(&socket_path_clone).await;
    assert!(result.is_ok(), "send_ctrl_alt_del should succeed: {:?}", result);

    // Verify the request format
    let request = server.await.unwrap();
    assert!(
        request.starts_with("PUT /actions HTTP/1.1\r\n"),
        "request should start with PUT /actions HTTP/1.1, got: {}",
        request
    );
    assert!(
        request.contains("Content-Type: application/json"),
        "request should contain Content-Type: application/json"
    );
    assert!(
        request.contains(r#"{"action_type":"SendCtrlAltDel"}"#),
        "request body should contain SendCtrlAltDel action"
    );
}

// ---------------------------------------------------------------------------
// Test 8: Shutdown cleans up runtime directory
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_shutdown_cleans_runtime_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let script = write_test_script(tmp.path(), "sleep 300");

    let config = SpawnConfig {
        firecracker_bin: script,
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let runtime_dir = process.runtime_dir().to_path_buf();
    assert!(runtime_dir.exists(), "runtime dir should exist before shutdown");

    let _report = process.shutdown(ShutdownConfig::default()).await;

    assert!(
        !runtime_dir.exists(),
        "runtime dir should be removed after shutdown"
    );
}

// ---------------------------------------------------------------------------
// Test 9: Custom timeouts work and shutdown completes quickly
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_custom_timeouts() {
    let tmp = tempfile::tempdir().unwrap();
    let script = write_test_script(tmp.path(), "sleep 300");

    let config = SpawnConfig {
        firecracker_bin: script,
        config_file: None,
        runtime_base_dir: Some(tmp.path().to_path_buf()),
        log_file: None,
    };

    let mut process = spawn(&config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let short_config = ShutdownConfig {
        ctrl_alt_del_timeout: Duration::from_millis(50),
        sigterm_timeout: Duration::from_millis(50),
        sigkill_timeout: Duration::from_secs(5),
    };

    let start = Instant::now();
    let report = process.shutdown(short_config).await;
    let elapsed = start.elapsed();

    // sleep responds to SIGTERM, so Sigterm stage
    assert_eq!(
        report.final_stage,
        ShutdownStage::Sigterm,
        "expected Sigterm with custom timeouts"
    );

    // Should complete quickly (ctrl_alt_del_timeout + some margin)
    // The CtrlAltDel connect will fail fast (no socket), SIGTERM works immediately
    assert!(
        elapsed < Duration::from_secs(3),
        "shutdown with short timeouts should be fast, took {:?}",
        elapsed
    );
}
