//! MinIO object storage integration: per-bundle namespace isolation via opendal.

pub mod client;
pub mod config;
pub mod error;

pub use client::MinioClient;
pub use config::StorageConfig;
pub use error::StorageError;
