//! Shutdown escalation logic and SendCtrlAltDel HTTP request.
//!
//! Provides graceful shutdown escalation (SendCtrlAltDel -> SIGTERM -> SIGKILL)
//! with configurable timeouts, and a raw HTTP `PUT /actions` request for
//! sending `CtrlAltDel` over a Firecracker API socket.

use std::path::Path;
use std::time::{Duration, Instant};

use hyphae_errors::ProcessError;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

use super::types::{ProcessState, ShutdownConfig, ShutdownReport, ShutdownStage};

/// Send a `SendCtrlAltDel` action to Firecracker over its Unix API socket.
///
/// Constructs and writes a raw HTTP PUT request:
/// ```text
/// PUT /actions HTTP/1.1\r\n
/// Host: localhost\r\n
/// Content-Type: application/json\r\n
/// Content-Length: {len}\r\n
/// \r\n
/// {"action_type":"SendCtrlAltDel"}
/// ```
///
/// Returns `Ok(())` if the response contains "200" or "204".
/// Returns `Err(ProcessError::SocketError)` on connection failure or bad response.
pub async fn send_ctrl_alt_del(socket_path: &Path) -> Result<(), ProcessError> {
    let mut stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .map_err(|e| ProcessError::SocketError(format!("connect failed: {e}")))?;

    let body = r#"{"action_type":"SendCtrlAltDel"}"#;
    let request = format!(
        "PUT /actions HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| ProcessError::SocketError(format!("write failed: {e}")))?;

    let mut buf = [0u8; 256];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| ProcessError::SocketError(format!("read failed: {e}")))?;

    let response = String::from_utf8_lossy(&buf[..n]);
    if response.contains("200") || response.contains("204") {
        Ok(())
    } else {
        Err(ProcessError::SocketError(format!(
            "unexpected response: {response}"
        )))
    }
}

/// Run the shutdown escalation sequence for a process.
///
/// Stages:
/// 1. Check if already exited -> return `AlreadyExited`
/// 2. Try `SendCtrlAltDel` via API socket, wait `ctrl_alt_del_timeout`
/// 3. Send `SIGTERM`, wait `sigterm_timeout`
/// 4. Send `SIGKILL`, wait `sigkill_timeout`
///
/// Returns a [`ShutdownReport`] with the final stage that stopped the process.
pub async fn shutdown_escalation(
    pid: u32,
    api_socket: &Path,
    state_rx: &mut watch::Receiver<ProcessState>,
    config: ShutdownConfig,
) -> ShutdownReport {
    let start = Instant::now();
    let nix_pid = Pid::from_raw(pid as i32);

    // Check if already exited
    {
        let state = state_rx.borrow_and_update().clone();
        if matches!(state, ProcessState::Exited(_)) {
            return ShutdownReport {
                final_stage: ShutdownStage::AlreadyExited,
                duration: start.elapsed(),
            };
        }
    }

    // Stage 1: Try SendCtrlAltDel
    match send_ctrl_alt_del(api_socket).await {
        Ok(()) => {
            tracing::debug!(pid, "sent SendCtrlAltDel, waiting for exit");
            if wait_for_exit(state_rx, config.ctrl_alt_del_timeout)
                .await
                .is_ok()
            {
                return ShutdownReport {
                    final_stage: ShutdownStage::CtrlAltDel,
                    duration: start.elapsed(),
                };
            }
            tracing::debug!(pid, "CtrlAltDel timeout, escalating to SIGTERM");
        }
        Err(e) => {
            tracing::debug!(pid, error = %e, "CtrlAltDel failed, skipping to SIGTERM");
        }
    }

    // Stage 2: SIGTERM
    if let Err(e) = signal::kill(nix_pid, Signal::SIGTERM) {
        tracing::debug!(pid, error = %e, "failed to send SIGTERM");
    } else {
        tracing::debug!(pid, "sent SIGTERM, waiting for exit");
        if wait_for_exit(state_rx, config.sigterm_timeout).await.is_ok() {
            return ShutdownReport {
                final_stage: ShutdownStage::Sigterm,
                duration: start.elapsed(),
            };
        }
        tracing::debug!(pid, "SIGTERM timeout, escalating to SIGKILL");
    }

    // Stage 3: SIGKILL
    if let Err(e) = signal::kill(nix_pid, Signal::SIGKILL) {
        tracing::debug!(pid, error = %e, "failed to send SIGKILL");
    } else {
        tracing::debug!(pid, "sent SIGKILL, waiting for exit");
        let _ = wait_for_exit(state_rx, config.sigkill_timeout).await;
    }

    ShutdownReport {
        final_stage: ShutdownStage::Sigkill,
        duration: start.elapsed(),
    }
}

/// Wait for a process to reach `Exited` state within a timeout.
///
/// Returns `Ok(())` if the process exits within the timeout, `Err(())` if it times out.
pub(crate) async fn wait_for_exit(
    rx: &mut watch::Receiver<ProcessState>,
    timeout: Duration,
) -> Result<(), ()> {
    tokio::time::timeout(timeout, async {
        loop {
            {
                let state = rx.borrow_and_update().clone();
                if matches!(state, ProcessState::Exited(_)) {
                    return;
                }
            }
            if rx.changed().await.is_err() {
                // Channel closed -- treat as exited
                return;
            }
        }
    })
    .await
    .map_err(|_| ())
}
