//! NamedVolumeStore implementation for PgSessionStore.
//!
//! Named volumes are persistent, user-owned volumes that can be referenced by
//! bundle manifests via `[volumes] shared_mount = "name"`. Exclusive access is
//! enforced via PgAdvisoryLock in the session handler, not by a DB lock column.

use async_trait::async_trait;
use sqlx_core::query::query;
use sqlx_core::query_as::query_as;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::NamedVolume;
use crate::postgres::PgSessionStore;
use crate::store::NamedVolumeStore;

#[async_trait]
impl NamedVolumeStore for PgSessionStore {
    async fn create_named_volume(&self, user_id: Uuid, name: &str, size_bytes: i64) -> Result<NamedVolume, StoreError> {
        self.create_named_volume_impl(user_id, name, size_bytes).await
    }

    async fn get_named_volume(&self, id: Uuid) -> Result<Option<NamedVolume>, StoreError> {
        self.get_named_volume_impl(id).await
    }

    async fn find_named_volume_by_name(&self, user_id: Uuid, name: &str) -> Result<Option<NamedVolume>, StoreError> {
        self.find_named_volume_by_name_impl(user_id, name).await
    }

    async fn list_named_volumes_for_user(&self, user_id: Uuid) -> Result<Vec<NamedVolume>, StoreError> {
        self.list_named_volumes_for_user_impl(user_id).await
    }

    async fn set_named_volume_worker(&self, id: Uuid, worker_id: Option<&str>) -> Result<(), StoreError> {
        self.set_named_volume_worker_impl(id, worker_id).await
    }

    async fn delete_named_volume(&self, id: Uuid, user_id: Uuid) -> Result<(), StoreError> {
        self.delete_named_volume_impl(id, user_id).await
    }
}

impl PgSessionStore {
    pub(crate) async fn create_named_volume_impl(
        &self,
        user_id: Uuid,
        name: &str,
        size_bytes: i64,
    ) -> Result<NamedVolume, StoreError> {
        let result = query_as::<_, NamedVolume>(
            r#"
            INSERT INTO named_volumes (user_id, name, size_bytes)
            VALUES ($1, $2, $3)
            RETURNING id, user_id, name, size_bytes, worker_id, created_at, updated_at
            "#,
        )
        .bind(user_id)
        .bind(name)
        .bind(size_bytes)
        .fetch_one(&self.pool)
        .await;

        match result {
            Ok(vol) => Ok(vol),
            Err(sqlx_core::Error::Database(ref dbe)) if dbe.code().as_deref() == Some("23505") => {
                Err(StoreError::Conflict(format!(
                    "named volume '{}' already exists for this user",
                    name
                )))
            }
            Err(e) => Err(StoreError::from(e)),
        }
    }

    pub(crate) async fn get_named_volume_impl(
        &self,
        id: Uuid,
    ) -> Result<Option<NamedVolume>, StoreError> {
        let vol = query_as::<_, NamedVolume>(
            r#"
            SELECT id, user_id, name, size_bytes, worker_id, created_at, updated_at
            FROM named_volumes
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(vol)
    }

    pub(crate) async fn find_named_volume_by_name_impl(
        &self,
        user_id: Uuid,
        name: &str,
    ) -> Result<Option<NamedVolume>, StoreError> {
        let vol = query_as::<_, NamedVolume>(
            r#"
            SELECT id, user_id, name, size_bytes, worker_id, created_at, updated_at
            FROM named_volumes
            WHERE user_id = $1 AND name = $2
            "#,
        )
        .bind(user_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(vol)
    }

    pub(crate) async fn list_named_volumes_for_user_impl(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<NamedVolume>, StoreError> {
        let vols = query_as::<_, NamedVolume>(
            r#"
            SELECT id, user_id, name, size_bytes, worker_id, created_at, updated_at
            FROM named_volumes
            WHERE user_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(vols)
    }

    pub(crate) async fn set_named_volume_worker_impl(
        &self,
        id: Uuid,
        worker_id: Option<&str>,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE named_volumes SET worker_id = $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(worker_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    pub(crate) async fn delete_named_volume_impl(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<(), StoreError> {
        let result = query(
            "DELETE FROM named_volumes WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            Err(StoreError::NamedVolumeNotFound { id })
        } else {
            Ok(())
        }
    }
}
