//! SecretStore implementation for PgSessionStore.

use sqlx_core::query::query;
use sqlx_core::row::Row;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::{SecretMetadata, TokenRecord};
use crate::postgres::PgSessionStore;

impl PgSessionStore {
    pub(crate) async fn upsert_secret_metadata_impl(
        &self,
        user_id: Uuid,
        bundle_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        query(
            "INSERT INTO bundle_secrets (user_id, bundle_id, name)
             VALUES ($1, $2, $3)
             ON CONFLICT (user_id, bundle_id, name)
             DO UPDATE SET updated_at = NOW()",
        )
        .bind(user_id)
        .bind(bundle_id)
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    pub(crate) async fn list_secret_metadata_impl(
        &self,
        user_id: Uuid,
        bundle_id: &str,
    ) -> Result<Vec<SecretMetadata>, StoreError> {
        let rows = query(
            "SELECT name, created_at, updated_at
             FROM bundle_secrets
             WHERE user_id = $1 AND bundle_id = $2
             ORDER BY name",
        )
        .bind(user_id)
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let secrets = rows
            .into_iter()
            .map(|row| {
                Ok(SecretMetadata {
                    name: row.try_get("name")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx_core::Error>>()
            .map_err(StoreError::from)?;

        Ok(secrets)
    }

    pub(crate) async fn delete_secret_metadata_impl(
        &self,
        user_id: Uuid,
        bundle_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        query(
            "DELETE FROM bundle_secrets
             WHERE user_id = $1 AND bundle_id = $2 AND name = $3",
        )
        .bind(user_id)
        .bind(bundle_id)
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    pub(crate) async fn insert_token_impl(
        &self,
        id: &str,
        bundle_id: &str,
        user_id: Uuid,
        ciphertext: &str,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), StoreError> {
        query(
            "INSERT INTO tokens (id, bundle_id, user_id, ciphertext, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(bundle_id)
        .bind(user_id)
        .bind(ciphertext)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    pub(crate) async fn get_token_impl(
        &self,
        id: &str,
        bundle_id: &str,
    ) -> Result<Option<TokenRecord>, StoreError> {
        let row = query(
            "SELECT id, bundle_id, user_id, ciphertext, created_at, expires_at
             FROM tokens
             WHERE id = $1 AND bundle_id = $2",
        )
        .bind(id)
        .bind(bundle_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        match row {
            None => Ok(None),
            Some(row) => Ok(Some(TokenRecord {
                id: row.try_get("id").map_err(StoreError::from)?,
                bundle_id: row.try_get("bundle_id").map_err(StoreError::from)?,
                user_id: row.try_get("user_id").map_err(StoreError::from)?,
                ciphertext: row.try_get("ciphertext").map_err(StoreError::from)?,
                created_at: row.try_get("created_at").map_err(StoreError::from)?,
                expires_at: row.try_get("expires_at").map_err(StoreError::from)?,
            })),
        }
    }

    pub(crate) async fn delete_expired_tokens_impl(&self) -> Result<u64, StoreError> {
        let result = query(
            "DELETE FROM tokens
             WHERE expires_at IS NOT NULL AND expires_at < NOW()",
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(result.rows_affected())
    }
}
