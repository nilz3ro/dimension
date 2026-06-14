//! ArtifactStore implementation for PgSessionStore.
//!
//! Provides artifact lifecycle management: object storage writes via opendal
//! combined with Postgres metadata tracking, per the locked decision that
//! ArtifactStore.put_artifact() does both writes in one call.

use async_trait::async_trait;
use bytes::Bytes;
use opendal::Operator;
use sqlx_core::query::query;
use sqlx_core::query_as::query_as;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::Artifact;
use crate::postgres::PgSessionStore;
use crate::store::ArtifactStore;

#[async_trait]
impl ArtifactStore for PgSessionStore {
    async fn put_artifact(
        &self,
        operator: &Operator,
        session_id: Uuid,
        user_id: Uuid,
        object_key: &str,
        data: Bytes,
        content_type: Option<&str>,
    ) -> Result<Artifact, StoreError> {
        self.put_artifact_impl(operator, session_id, user_id, object_key, data, content_type).await
    }

    async fn list_artifacts_for_session(
        &self,
        session_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<Artifact>, StoreError> {
        self.list_artifacts_for_session_impl(session_id, user_id).await
    }

    async fn get_artifact(
        &self,
        session_id: Uuid,
        user_id: Uuid,
        object_key: &str,
    ) -> Result<Option<Artifact>, StoreError> {
        self.get_artifact_impl(session_id, user_id, object_key).await
    }

    async fn delete_artifact(
        &self,
        operator: &Operator,
        session_id: Uuid,
        user_id: Uuid,
        object_key: &str,
    ) -> Result<(), StoreError> {
        self.delete_artifact_impl(operator, session_id, user_id, object_key).await
    }

    async fn list_artifacts_for_user(
        &self,
        user_id: Uuid,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Artifact>, Option<String>), StoreError> {
        self.list_artifacts_for_user_impl(user_id, cursor, limit).await
    }

    async fn admin_list_artifacts(
        &self,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Artifact>, Option<String>), StoreError> {
        self.admin_list_artifacts_impl(cursor, limit).await
    }

    async fn delete_expired_artifacts(
        &self,
        operator: &Operator,
    ) -> Result<u64, StoreError> {
        self.delete_expired_artifacts_impl(operator).await
    }
}

impl PgSessionStore {
    /// Write artifact bytes to object storage first, then insert Postgres metadata.
    /// On Postgres failure after successful object storage write, performs best-effort
    /// cleanup of the object storage entry before returning the error.
    pub(crate) async fn put_artifact_impl(
        &self,
        operator: &Operator,
        session_id: Uuid,
        user_id: Uuid,
        object_key: &str,
        data: Bytes,
        content_type: Option<&str>,
    ) -> Result<Artifact, StoreError> {
        let size_bytes = data.len() as i64;

        // Compute SHA-256 checksum for deduplication and integrity verification.
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(&data);
        let checksum = hex::encode(digest);

        // Step 1: Write to object storage first (per locked decision ordering).
        operator
            .write(object_key, data)
            .await
            .map_err(|e| StoreError::ObjectStorage(e.to_string()))?;

        // Step 2: Upsert Postgres metadata (UPSERT per CONTEXT.md locked decision:
        // "Overwritable: publishing to the same key replaces the content -- latest version wins").
        // ON CONFLICT (session_id, object_key) DO UPDATE so republishing the same key
        // overwrites the metadata row. The opendal write() call already overwrites in S3.
        // On Postgres failure after successful object storage write, performs best-effort
        // cleanup of the object storage entry before returning the error.
        let pg_result = query_as::<_, Artifact>(
            r#"
            INSERT INTO artifacts (session_id, user_id, object_key, size_bytes, content_type, checksum)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (session_id, object_key) DO UPDATE SET
              size_bytes = EXCLUDED.size_bytes,
              content_type = EXCLUDED.content_type,
              checksum = EXCLUDED.checksum
            RETURNING id, session_id, user_id, object_key, size_bytes, content_type, checksum, created_at, expires_at
            "#,
        )
        .bind(session_id)
        .bind(user_id)
        .bind(object_key)
        .bind(size_bytes)
        .bind(content_type)
        .bind(&checksum)
        .fetch_one(&self.pool)
        .await;

        match pg_result {
            Ok(artifact) => Ok(artifact),
            Err(e) => {
                // Best-effort cleanup: delete the object we just wrote.
                let _ = operator.delete(object_key).await;
                Err(StoreError::from(e))
            }
        }
    }

    pub(crate) async fn list_artifacts_for_session_impl(
        &self,
        session_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<Artifact>, StoreError> {
        let artifacts = query_as::<_, Artifact>(
            r#"
            SELECT id, session_id, user_id, object_key, size_bytes, content_type, checksum, created_at, expires_at
            FROM artifacts
            WHERE session_id = $1 AND user_id = $2
            ORDER BY created_at DESC
            "#,
        )
        .bind(session_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(artifacts)
    }

    pub(crate) async fn get_artifact_impl(
        &self,
        session_id: Uuid,
        user_id: Uuid,
        object_key: &str,
    ) -> Result<Option<Artifact>, StoreError> {
        let artifact = query_as::<_, Artifact>(
            r#"
            SELECT id, session_id, user_id, object_key, size_bytes, content_type, checksum, created_at, expires_at
            FROM artifacts
            WHERE session_id = $1 AND user_id = $2 AND object_key = $3
            "#,
        )
        .bind(session_id)
        .bind(user_id)
        .bind(object_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(artifact)
    }

    /// List artifact metadata for a user across all sessions, keyset-paginated by (created_at DESC, id DESC).
    ///
    /// The cursor is the UUID string of the last artifact from the previous page.
    /// Uses the existing idx_artifacts_user index on (user_id, created_at DESC) for efficient access.
    pub(crate) async fn list_artifacts_for_user_impl(
        &self,
        user_id: Uuid,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Artifact>, Option<String>), StoreError> {
        // Parse cursor UUID if provided; invalid cursor yields empty page.
        let cursor_id: Option<Uuid> = match cursor {
            Some(c) => Some(
                Uuid::parse_str(c)
                    .map_err(|_| StoreError::InvalidCursor { reason: "cursor is not a valid UUID".into() })?,
            ),
            None => None,
        };

        let artifacts = query_as::<_, Artifact>(
            r#"
            SELECT id, session_id, user_id, object_key, size_bytes, content_type, checksum, created_at, expires_at
            FROM artifacts
            WHERE user_id = $1
              AND ($2::uuid IS NULL OR (created_at, id) < (SELECT created_at, id FROM artifacts WHERE id = $2))
            ORDER BY created_at DESC, id DESC
            LIMIT $3
            "#,
        )
        .bind(user_id)
        .bind(cursor_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        // Return cursor as the id of the last row when results equal limit.
        let next_cursor = if artifacts.len() as i64 == limit {
            artifacts.last().map(|a| a.id.to_string())
        } else {
            None
        };

        Ok((artifacts, next_cursor))
    }

    /// Admin: list all artifacts across all users, keyset-paginated by (created_at DESC, id DESC).
    ///
    /// The cursor is the UUID string of the last artifact from the previous page.
    pub(crate) async fn admin_list_artifacts_impl(
        &self,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Artifact>, Option<String>), StoreError> {
        let cursor_id: Option<Uuid> = match cursor {
            Some(c) => Some(
                Uuid::parse_str(c)
                    .map_err(|_| StoreError::InvalidCursor { reason: "cursor is not a valid UUID".into() })?,
            ),
            None => None,
        };

        let artifacts = query_as::<_, Artifact>(
            r#"
            SELECT id, session_id, user_id, object_key, size_bytes, content_type, checksum, created_at, expires_at
            FROM artifacts
            WHERE ($1::uuid IS NULL OR (created_at, id) < (SELECT created_at, id FROM artifacts WHERE id = $1))
            ORDER BY created_at DESC, id DESC
            LIMIT $2
            "#,
        )
        .bind(cursor_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let next_cursor = if artifacts.len() as i64 == limit {
            artifacts.last().map(|a| a.id.to_string())
        } else {
            None
        };

        Ok((artifacts, next_cursor))
    }

    /// Delete an artifact from object storage first, then remove the Postgres row.
    pub(crate) async fn delete_artifact_impl(
        &self,
        operator: &Operator,
        session_id: Uuid,
        user_id: Uuid,
        object_key: &str,
    ) -> Result<(), StoreError> {
        // Step 1: Delete from object storage first.
        operator
            .delete(object_key)
            .await
            .map_err(|e| StoreError::ObjectStorage(e.to_string()))?;

        // Step 2: Delete Postgres metadata row.
        sqlx_core::query::query(
            "DELETE FROM artifacts WHERE session_id = $1 AND user_id = $2 AND object_key = $3",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(object_key)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    /// Delete all artifacts where expires_at < now().
    ///
    /// For each expired artifact:
    /// 1. Delete from object storage (best-effort, logs warning on failure).
    /// 2. Delete the Postgres row regardless of object storage result.
    ///
    /// Skips artifacts that fail to delete from Postgres (logs warning, continues).
    /// Returns the count of artifacts successfully deleted from Postgres.
    pub(crate) async fn delete_expired_artifacts_impl(
        &self,
        operator: &Operator,
    ) -> Result<u64, StoreError> {
        // Fetch expired artifact metadata
        let expired: Vec<Artifact> = query_as(
            "SELECT id, session_id, user_id, object_key, size_bytes, content_type, checksum, created_at, expires_at
             FROM artifacts WHERE expires_at IS NOT NULL AND expires_at < now()"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let mut deleted = 0u64;
        for artifact in expired {
            // Delete from object storage first (best-effort, log on failure)
            if let Err(e) = operator.delete(&artifact.object_key).await {
                tracing::warn!(
                    artifact_id = %artifact.id,
                    key = %artifact.object_key,
                    error = %e,
                    "failed to delete expired artifact from object storage"
                );
            }
            // Delete from Postgres regardless of object storage result
            if let Err(e) = query("DELETE FROM artifacts WHERE id = $1")
                .bind(artifact.id)
                .execute(&self.pool)
                .await
            {
                tracing::warn!(artifact_id = %artifact.id, error = %e, "failed to delete artifact row");
                continue;
            }
            deleted += 1;
        }
        Ok(deleted)
    }
}
