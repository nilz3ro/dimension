//! S3 kernel listing and download with streaming SHA-256 and retry logic.
//!
//! Kernels are downloaded from the Firecracker CI S3 bucket, which hosts
//! pre-built vmlinux binaries for each Firecracker release and architecture.

use crate::kernel::types::{AvailableKernel, KernelDownloadResult};
use hyphae_errors::KernelError;
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;

const S3_BUCKET_URL: &str = "https://s3.amazonaws.com/spec.ccfc.min";
const S3_LIST_URL: &str = "http://spec.ccfc.min.s3.amazonaws.com";
const MAX_RETRIES: u32 = 3;

/// Create a reqwest [`Client`] configured for kernel downloads.
pub fn create_http_client() -> Result<Client, KernelError> {
    Client::builder()
        .timeout(Duration::from_secs(300)) // 5 min download timeout
        .connect_timeout(Duration::from_secs(10)) // 10 sec connect timeout
        .build()
        .map_err(|e| KernelError::NetworkError {
            message: e.to_string(),
        })
}

/// List available kernels from the Firecracker CI S3 bucket.
///
/// Queries the bucket with a prefix matching the given Firecracker version
/// and host architecture, then parses the S3 XML listing response.
pub async fn list_available_kernels(
    client: &Client,
    fc_version: &str,
    arch: &str,
) -> Result<Vec<AvailableKernel>, KernelError> {
    let prefix = format!("firecracker-ci/{}/{}/vmlinux-", fc_version, arch);
    let url = format!("{}/?prefix={}&list-type=2", S3_LIST_URL, prefix);

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| KernelError::NetworkError {
            message: e.to_string(),
        })?;

    if !response.status().is_success() {
        return Err(KernelError::DownloadFailed {
            url,
            status: response.status().as_u16(),
        });
    }

    let body = response
        .text()
        .await
        .map_err(|e| KernelError::NetworkError {
            message: e.to_string(),
        })?;

    parse_s3_listing(&body)
}

/// Parse an S3 ListObjectsV2 XML response to extract kernel versions.
///
/// Uses simple string splitting rather than a full XML parser to avoid
/// adding a dependency for this single use case.
fn parse_s3_listing(xml: &str) -> Result<Vec<AvailableKernel>, KernelError> {
    let mut kernels = Vec::new();

    for key_match in xml.split("<Key>").skip(1) {
        let key = key_match
            .split("</Key>")
            .next()
            .ok_or(KernelError::MalformedS3Response)?;

        // Skip .config files and other non-kernel files.
        if key.contains(".config") || key.contains("-no-acpi") {
            continue;
        }

        // Extract version from key like "firecracker-ci/v1.14/x86_64/vmlinux-6.1.155".
        let filename = key.rsplit('/').next().unwrap_or("");
        let version = match filename.strip_prefix("vmlinux-") {
            Some(v) => v,
            None => continue, // Skip non-vmlinux files.
        };

        // Extract size if present in the same <Contents> block.
        let size = key_match
            .split("<Size>")
            .nth(1)
            .and_then(|s| s.split("</Size>").next())
            .and_then(|s| s.parse::<u64>().ok());

        kernels.push(AvailableKernel {
            version: version.to_string(),
            s3_key: key.to_string(),
            size_bytes: size,
        });
    }

    // Sort by semantic version (ascending -- latest is last).
    kernels.sort_by(|a, b| version_cmp(&a.version, &b.version));

    Ok(kernels)
}

/// Compare kernel version strings like "6.1.155" and "5.10.245".
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |v: &str| -> Vec<u32> {
        v.split('.').filter_map(|s| s.parse().ok()).collect()
    };
    parse(a).cmp(&parse(b))
}

/// Download a kernel with retry logic.
///
/// Downloads to the destination path, computes SHA-256 during download,
/// and retries up to [`MAX_RETRIES`] times on network errors and 5xx
/// responses with exponential backoff (1s, 2s, 4s).
pub async fn download_kernel(
    client: &Client,
    kernel: &AvailableKernel,
    dest: &Path,
) -> Result<KernelDownloadResult, KernelError> {
    let url = format!("{}/{}", S3_BUCKET_URL, kernel.s3_key);
    let mut attempt = 0u32;

    loop {
        match download_kernel_inner(client, &url, dest).await {
            Ok(result) => return Ok(result),
            Err(e) if attempt < MAX_RETRIES && is_retryable(&e) => {
                attempt += 1;
                let delay = Duration::from_secs(1 << (attempt - 1)); // 1s, 2s, 4s
                // Clean up partial download before retry.
                let _ = tokio::fs::remove_file(dest).await;
                tokio::time::sleep(delay).await;
            }
            Err(e) => {
                // Clean up partial download on final failure.
                let _ = tokio::fs::remove_file(dest).await;
                return Err(e);
            }
        }
    }
}

/// Determine if a [`KernelError`] is retryable (network errors and 5xx).
fn is_retryable(e: &KernelError) -> bool {
    match e {
        KernelError::NetworkError { .. } => true,
        KernelError::DownloadFailed { status, .. } => *status >= 500,
        _ => false,
    }
}

/// Stream response to file while computing SHA-256.
async fn download_kernel_inner(
    client: &Client,
    url: &str,
    dest: &Path,
) -> Result<KernelDownloadResult, KernelError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| KernelError::NetworkError {
            message: e.to_string(),
        })?;

    if !response.status().is_success() {
        return Err(KernelError::DownloadFailed {
            url: url.to_string(),
            status: response.status().as_u16(),
        });
    }

    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| KernelError::IoError {
            path: dest.to_path_buf(),
            source: e,
        })?;
    let mut hasher = Sha256::new();
    let mut total_bytes: u64 = 0;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| KernelError::NetworkError {
            message: e.to_string(),
        })?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| KernelError::IoError {
                path: dest.to_path_buf(),
                source: e,
            })?;
        total_bytes += chunk.len() as u64;
    }

    file.flush()
        .await
        .map_err(|e| KernelError::IoError {
            path: dest.to_path_buf(),
            source: e,
        })?;

    let content_hash = hex::encode(hasher.finalize());

    Ok(KernelDownloadResult {
        path: dest.to_path_buf(),
        content_hash,
        size_bytes: total_bytes,
    })
}
