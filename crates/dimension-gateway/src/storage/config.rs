//! Storage configuration parsed from CLI flags with env var fallback.

/// Configuration for the MinIO object storage client.
#[derive(Debug, Clone, clap::Args)]
pub struct StorageConfig {
    /// MinIO server endpoint URL.
    #[arg(long, default_value = "http://127.0.0.1:9000", env = "MINIO_ENDPOINT")]
    pub endpoint: String,

    /// MinIO bucket name for all object storage operations.
    #[arg(long, default_value = "dimension", env = "MINIO_BUCKET")]
    pub bucket: String,

    /// MinIO access key ID for authentication.
    #[arg(long, env = "MINIO_ACCESS_KEY")]
    pub access_key: Option<String>,

    /// MinIO secret access key for authentication.
    #[arg(long, env = "MINIO_SECRET_KEY")]
    pub secret_key: Option<String>,
}
