//! MinIO client using opendal with per-bundle namespace isolation.

use opendal::{services::S3, Operator};

use crate::storage::config::StorageConfig;
use crate::storage::error::StorageError;

/// Client for MinIO object storage.
///
/// Provides per-bundle namespace isolation via opendal S3 root scoping.
/// The `operator_for` method returns an operator rooted at `/{user_id}/{bundle_id}/`,
/// which structurally prevents cross-bundle access at the storage layer.
pub struct MinioClient {
    endpoint: String,
    bucket: String,
    access_key: String,
    secret_key: String,
}

impl MinioClient {
    /// Connect to MinIO using the provided configuration.
    ///
    /// Returns `StorageError::NotConfigured` if `access_key` or `secret_key`
    /// is absent. This is the normal degraded-mode path when env vars are not set.
    pub fn connect(config: StorageConfig) -> Result<Self, StorageError> {
        let access_key = config.access_key.ok_or(StorageError::NotConfigured)?;
        let secret_key = config.secret_key.ok_or(StorageError::NotConfigured)?;

        Ok(Self {
            endpoint: config.endpoint,
            bucket: config.bucket,
            access_key,
            secret_key,
        })
    }

    /// Build an opendal operator scoped to the given user+bundle namespace.
    ///
    /// The root is set to `/{user_id}/{bundle_id}/` which means all paths
    /// within this operator are relative to that prefix. Cross-bundle access
    /// is structurally impossible through this operator.
    pub fn operator_for(&self, user_id: &str, bundle_id: &str) -> Result<Operator, StorageError> {
        let root = format!("/{}/{}/", user_id, bundle_id);
        let builder = S3::default()
            .endpoint(&self.endpoint)
            .region("auto")
            .bucket(&self.bucket)
            .access_key_id(&self.access_key)
            .secret_access_key(&self.secret_key)
            .root(&root);

        let operator = Operator::new(builder)?.finish();
        Ok(operator)
    }

    /// Build an opendal operator scoped to a user+bundle+session namespace.
    ///
    /// The root is set to `/{user_id}/{bundle_id}/{session_id}/` which
    /// isolates artifacts per conversation session.
    pub fn operator_for_session(
        &self,
        user_id: &str,
        bundle_id: &str,
        session_id: &str,
    ) -> Result<Operator, StorageError> {
        let root = format!("/{}/{}/{}/", user_id, bundle_id, session_id);
        let builder = S3::default()
            .endpoint(&self.endpoint)
            .region("auto")
            .bucket(&self.bucket)
            .access_key_id(&self.access_key)
            .secret_access_key(&self.secret_key)
            .root(&root);

        let operator = Operator::new(builder)?.finish();
        Ok(operator)
    }

    /// Build an opendal operator scoped to the root of the bucket.
    ///
    /// Used by admin endpoints that need to access all namespaces.
    pub fn admin_operator(&self) -> Result<Operator, StorageError> {
        let builder = S3::default()
            .endpoint(&self.endpoint)
            .region("auto")
            .bucket(&self.bucket)
            .access_key_id(&self.access_key)
            .secret_access_key(&self.secret_key)
            .root("/");

        let operator = Operator::new(builder)?.finish();
        Ok(operator)
    }
}
