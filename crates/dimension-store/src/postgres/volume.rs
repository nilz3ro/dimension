//! VolumeStore implementation for PgSessionStore.
//!
//! Provides CRUD and lifecycle operations for persistent volumes.

use async_trait::async_trait;
use sqlx_core::query::query;
use sqlx_core::query_as::query_as;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::Volume;
use crate::postgres::PgSessionStore;
use crate::store::VolumeStore;

#[async_trait]
impl VolumeStore for PgSessionStore {
    async fn create_volume(&self, user_id: Uuid, size_bytes: i64) -> Result<Volume, StoreError> {
        self.create_volume_impl(user_id, size_bytes).await
    }

    async fn get_volume(&self, volume_id: Uuid) -> Result<Option<Volume>, StoreError> {
        self.get_volume_impl(volume_id).await
    }

    async fn list_volumes_for_user(&self, user_id: Uuid) -> Result<Vec<Volume>, StoreError> {
        self.list_volumes_for_user_impl(user_id).await
    }

    async fn attach_volume(&self, volume_id: Uuid, session_id: Uuid) -> Result<Volume, StoreError> {
        self.attach_volume_impl(volume_id, session_id).await
    }

    async fn detach_volume(&self, volume_id: Uuid) -> Result<(), StoreError> {
        self.detach_volume_impl(volume_id).await
    }

    async fn set_worker(&self, volume_id: Uuid, worker_id: Option<&str>) -> Result<(), StoreError> {
        self.set_worker_impl(volume_id, worker_id).await
    }

    async fn delete_volume(&self, volume_id: Uuid, user_id: Uuid) -> Result<(), StoreError> {
        self.delete_volume_impl(volume_id, user_id).await
    }

    async fn find_volume_for_session(&self, session_id: Uuid) -> Result<Option<Volume>, StoreError> {
        self.find_volume_for_session_impl(session_id).await
    }

    async fn touch_volume(&self, volume_id: Uuid) -> Result<(), StoreError> {
        self.touch_volume_impl(volume_id).await
    }
}

impl PgSessionStore {
    pub(crate) async fn create_volume_impl(
        &self,
        user_id: Uuid,
        size_bytes: i64,
    ) -> Result<Volume, StoreError> {
        let volume = query_as::<_, Volume>(
            r#"
            INSERT INTO volumes (user_id, size_bytes)
            VALUES ($1, $2)
            RETURNING id, user_id, size_bytes, session_id, worker_id, created_at, updated_at, last_accessed
            "#,
        )
        .bind(user_id)
        .bind(size_bytes)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(volume)
    }

    pub(crate) async fn get_volume_impl(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<Volume>, StoreError> {
        let volume = query_as::<_, Volume>(
            r#"
            SELECT id, user_id, size_bytes, session_id, worker_id, created_at, updated_at, last_accessed
            FROM volumes
            WHERE id = $1
            "#,
        )
        .bind(volume_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(volume)
    }

    pub(crate) async fn list_volumes_for_user_impl(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<Volume>, StoreError> {
        let volumes = query_as::<_, Volume>(
            r#"
            SELECT id, user_id, size_bytes, session_id, worker_id, created_at, updated_at, last_accessed
            FROM volumes
            WHERE user_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(volumes)
    }

    /// Atomically attach a volume to a session.
    /// Uses UPDATE...WHERE session_id IS NULL to prevent concurrent attach race.
    /// Returns Conflict if the volume is already attached.
    pub(crate) async fn attach_volume_impl(
        &self,
        volume_id: Uuid,
        session_id: Uuid,
    ) -> Result<Volume, StoreError> {
        let volume = query_as::<_, Volume>(
            r#"
            UPDATE volumes
            SET session_id = $1, updated_at = NOW()
            WHERE id = $2 AND session_id IS NULL
            RETURNING id, user_id, size_bytes, session_id, worker_id, created_at, updated_at, last_accessed
            "#,
        )
        .bind(session_id)
        .bind(volume_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        match volume {
            Some(v) => Ok(v),
            None => Err(StoreError::Conflict(
                "volume already attached to a session".to_string(),
            )),
        }
    }

    pub(crate) async fn detach_volume_impl(
        &self,
        volume_id: Uuid,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE volumes SET session_id = NULL, updated_at = NOW() WHERE id = $1",
        )
        .bind(volume_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    pub(crate) async fn set_worker_impl(
        &self,
        volume_id: Uuid,
        worker_id: Option<&str>,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE volumes SET worker_id = $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(worker_id)
        .bind(volume_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    /// Find the volume attached to a given session. Returns None if no volume is attached.
    pub(crate) async fn find_volume_for_session_impl(
        &self,
        session_id: Uuid,
    ) -> Result<Option<Volume>, StoreError> {
        let volume = query_as::<_, Volume>(
            r#"
            SELECT id, user_id, size_bytes, session_id, worker_id, created_at, updated_at, last_accessed
            FROM volumes
            WHERE session_id = $1
            "#,
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(volume)
    }

    /// Update the last_accessed timestamp for a volume.
    /// Only touches last_accessed; does NOT change updated_at.
    pub(crate) async fn touch_volume_impl(
        &self,
        volume_id: Uuid,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE volumes SET last_accessed = NOW() WHERE id = $1",
        )
        .bind(volume_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    /// Delete a volume. Only succeeds if not currently attached to a session.
    /// Returns Conflict if attached, VolumeNotFound if not found.
    pub(crate) async fn delete_volume_impl(
        &self,
        volume_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), StoreError> {
        let result = query(
            "DELETE FROM volumes WHERE id = $1 AND user_id = $2 AND session_id IS NULL",
        )
        .bind(volume_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            // Check if the volume exists (may be attached or not found)
            let exists: Option<(Uuid,)> = sqlx_core::query_as::query_as(
                "SELECT id FROM volumes WHERE id = $1 AND user_id = $2",
            )
            .bind(volume_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::from)?;

            if exists.is_some() {
                Err(StoreError::Conflict(
                    "cannot delete volume that is currently attached to a session".to_string(),
                ))
            } else {
                Err(StoreError::VolumeNotFound { id: volume_id })
            }
        } else {
            Ok(())
        }
    }
}
