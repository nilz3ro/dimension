//! Raw HTTP-over-Unix-socket request functions for MMDS API.
//!
//! Provides low-level HTTP request/response functions that communicate with
//! the Firecracker API socket. Follows the same raw-HTTP-over-Unix-socket
//! pattern as Phase 3's `SendCtrlAltDel` in `process/shutdown.rs`.
//!
//! Each call opens a new Unix socket connection, sends one HTTP request,
//! reads one response, and closes -- Firecracker accepts one request per
//! connection.

use std::path::Path;

use hyphae_errors::MmdsError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Send an HTTP request to the Firecracker API socket and check for success.
///
/// Used for PUT requests to `/mmds/config` and `/mmds`. Returns `Ok(())`
/// if the response contains "204" or "200" (Firecracker returns 204 No
/// Content for successful PUT/PATCH operations).
pub async fn send_mmds_request(
    socket_path: &Path,
    method: &str,
    path: &str,
    body: &str,
) -> Result<(), MmdsError> {
    let response = send_mmds_request_raw(socket_path, method, path, body).await?;

    if response.contains("204") || response.contains("200") {
        Ok(())
    } else {
        Err(MmdsError::ApiError {
            endpoint: format!("{method} {path}"),
            response,
        })
    }
}

/// Send an HTTP request and return the response body (for GET requests).
///
/// Returns the body portion of the HTTP response (everything after the
/// `\r\n\r\n` header/body separator). Returns an error if the status code
/// is not 200 or 204.
pub async fn send_mmds_request_with_body(
    socket_path: &Path,
    method: &str,
    path: &str,
    body: &str,
) -> Result<String, MmdsError> {
    let response = send_mmds_request_raw(socket_path, method, path, body).await?;

    let is_success = response.contains("200") || response.contains("204");
    if !is_success {
        return Err(MmdsError::ApiError {
            endpoint: format!("{method} {path}"),
            response,
        });
    }

    // Extract body from HTTP response (after the double CRLF)
    let body_start = response.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
    Ok(response[body_start..].to_string())
}

/// Send a raw HTTP request over the Unix socket and return the raw response.
///
/// Constructs an HTTP/1.1 request with JSON content type, sends it over a
/// fresh `UnixStream` connection, shuts down the write half to signal
/// end-of-request, then reads the full response via `read_to_end`.
async fn send_mmds_request_raw(
    socket_path: &Path,
    method: &str,
    path: &str,
    body: &str,
) -> Result<String, MmdsError> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| MmdsError::SocketConnect {
            path: socket_path.to_path_buf(),
            message: e.to_string(),
        })?;

    let request = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Accept: application/json\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len(),
    );

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| MmdsError::SocketWrite(e.to_string()))?;

    // Read the response. DO NOT call shutdown() — Firecracker's API
    // socket RSTs the connection if the client half-closes.
    // Instead, read a fixed buffer (FC responses are small) with a timeout.
    let mut response_buf = vec![0u8; 8192];
    let n = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read(&mut response_buf),
    )
    .await
    {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => return Err(MmdsError::SocketRead(e.to_string())),
        Err(_) => {
            return Err(MmdsError::SocketRead(
                "timed out waiting for Firecracker API response".to_string(),
            ))
        }
    };
    response_buf.truncate(n);

    Ok(String::from_utf8_lossy(&response_buf).into_owned())
}
