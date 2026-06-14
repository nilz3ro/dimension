//! StorageStore implementation for PgSessionStore.

use async_trait::async_trait;
use sqlx_core::query::query;
use sqlx_core::row::Row;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::BundleStorageRecord;
use crate::postgres::PgSessionStore;
use crate::store::StorageStore;

impl PgSessionStore {
    /// Ensure a bundle_storage row exists, inserting with defaults if absent.
    async fn ensure_storage_row(&self, user_id: Uuid, bundle_id: &str) -> Result<(), StoreError> {
        query(
            "INSERT INTO bundle_storage (user_id, bundle_id)
             VALUES ($1, $2)
             ON CONFLICT (user_id, bundle_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(bundle_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;
        Ok(())
    }
}

#[async_trait]
impl StorageStore for PgSessionStore {
    async fn get_storage_info(&self, user_id: Uuid, bundle_id: &str) -> Result<(i64, i64), StoreError> {
        // Ensure row exists with defaults before reading.
        self.ensure_storage_row(user_id, bundle_id).await?;

        let row = query(
            "SELECT bytes_used, quota_bytes
             FROM bundle_storage
             WHERE user_id = $1 AND bundle_id = $2",
        )
        .bind(user_id)
        .bind(bundle_id)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let bytes_used: i64 = row.try_get("bytes_used").map_err(StoreError::from)?;
        let quota_bytes: i64 = row.try_get("quota_bytes").map_err(StoreError::from)?;
        Ok((bytes_used, quota_bytes))
    }

    async fn increment_bytes_used(
        &self,
        user_id: Uuid,
        bundle_id: &str,
        delta: i64,
    ) -> Result<i64, StoreError> {
        // Ensure row exists before updating.
        self.ensure_storage_row(user_id, bundle_id).await?;

        let row = query(
            "UPDATE bundle_storage
             SET bytes_used = bytes_used + $3, updated_at = NOW()
             WHERE user_id = $1 AND bundle_id = $2
             RETURNING bytes_used",
        )
        .bind(user_id)
        .bind(bundle_id)
        .bind(delta)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let bytes_used: i64 = row.try_get("bytes_used").map_err(StoreError::from)?;
        Ok(bytes_used)
    }

    async fn decrement_bytes_used(
        &self,
        user_id: Uuid,
        bundle_id: &str,
        delta: i64,
    ) -> Result<(), StoreError> {
        // Ensure row exists before updating. Floor bytes_used at zero.
        self.ensure_storage_row(user_id, bundle_id).await?;

        query(
            "UPDATE bundle_storage
             SET bytes_used = GREATEST(bytes_used - $3, 0), updated_at = NOW()
             WHERE user_id = $1 AND bundle_id = $2",
        )
        .bind(user_id)
        .bind(bundle_id)
        .bind(delta)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    async fn set_quota(
        &self,
        user_id: Uuid,
        bundle_id: &str,
        quota_bytes: i64,
    ) -> Result<(), StoreError> {
        // Ensure row exists before updating.
        self.ensure_storage_row(user_id, bundle_id).await?;

        query(
            "UPDATE bundle_storage
             SET quota_bytes = $3, updated_at = NOW()
             WHERE user_id = $1 AND bundle_id = $2",
        )
        .bind(user_id)
        .bind(bundle_id)
        .bind(quota_bytes)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    async fn list_storage_stats(&self) -> Result<Vec<BundleStorageRecord>, StoreError> {
        let rows = query(
            "SELECT id, user_id, bundle_id, bytes_used, quota_bytes, updated_at
             FROM bundle_storage
             ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let records = rows
            .into_iter()
            .map(|row| {
                Ok(BundleStorageRecord {
                    id: row.try_get("id")?,
                    user_id: row.try_get("user_id")?,
                    bundle_id: row.try_get("bundle_id")?,
                    bytes_used: row.try_get("bytes_used")?,
                    quota_bytes: row.try_get("quota_bytes")?,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx_core::Error>>()
            .map_err(StoreError::from)?;

        Ok(records)
    }

    async fn set_bytes_used(
        &self,
        user_id: Uuid,
        bundle_id: &str,
        bytes: i64,
    ) -> Result<(), StoreError> {
        // Ensure row exists before updating.
        self.ensure_storage_row(user_id, bundle_id).await?;

        query(
            "UPDATE bundle_storage
             SET bytes_used = $3, updated_at = NOW()
             WHERE user_id = $1 AND bundle_id = $2",
        )
        .bind(user_id)
        .bind(bundle_id)
        .bind(bytes)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }
}
