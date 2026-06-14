//! Error types for the MinIO storage subsystem.

use thiserror::Error;

/// Errors that can occur in the storage subsystem.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Storage is not configured (access key or secret key not provided).
    /// This is the normal case when MinIO env vars are absent.
    #[error("storage not configured (MINIO_ACCESS_KEY / MINIO_SECRET_KEY not set)")]
    NotConfigured,

    /// An opendal operation failed.
    #[error("opendal error: {0}")]
    Opendal(#[from] opendal::Error),

    /// The bundle has exceeded its storage quota.
    #[error("storage quota exceeded: {0}")]
    QuotaExceeded(String),
}
