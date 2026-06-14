//! Kernel type definitions for registry records and S3 discovery.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A kernel record stored in the registry database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelRecord {
    pub id: i64,
    pub version: String,
    pub content_hash: String,
    pub source: KernelSource,
    pub arch: String,
    pub disk_path: PathBuf,
    pub size_bytes: u64,
    pub fc_version: Option<String>,
    pub created_at: i64,
}

/// Whether a kernel was downloaded from S3 or provided by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelSource {
    Managed,
    Custom,
}

impl std::fmt::Display for KernelSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelSource::Managed => write!(f, "managed"),
            KernelSource::Custom => write!(f, "custom"),
        }
    }
}

impl KernelSource {
    pub fn from_str_value(s: &str) -> Result<Self, String> {
        match s {
            "managed" => Ok(KernelSource::Managed),
            "custom" => Ok(KernelSource::Custom),
            other => Err(format!("Unknown kernel source: {}", other)),
        }
    }
}

/// A kernel version discovered in the Firecracker CI S3 bucket.
#[derive(Debug, Clone)]
pub struct AvailableKernel {
    pub version: String,
    pub s3_key: String,
    pub size_bytes: Option<u64>,
}

/// Result from a kernel download operation.
pub struct KernelDownloadResult {
    pub path: PathBuf,
    pub content_hash: String,
    pub size_bytes: u64,
}
