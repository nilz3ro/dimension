use sqlx_core::query::query;
use sqlx_core::query_as::query_as;
use uuid::Uuid;

use crate::crypto::{generate_api_key, hash_api_key};
use crate::error::StoreError;
use crate::models::{ApiKey, AuthenticatedUser, UserRole};
use crate::postgres::PgSessionStore;

impl PgSessionStore {
    /// Create an API key for a user. Returns (ApiKey metadata, plaintext key).
    pub(crate) async fn create_key_impl(
        &self,
        user_id: Uuid,
        label: Option<&str>,
    ) -> Result<(ApiKey, String), StoreError> {
        let (plaintext, prefix) = generate_api_key();
        let hash = hash_api_key(&plaintext);

        let api_key = query_as::<_, ApiKey>(
            "INSERT INTO api_keys (user_id, key_hash, key_prefix, label) \
             VALUES ($1, $2, $3, $4) \
             RETURNING id, user_id, key_hash, key_prefix, label, created_at, revoked_at",
        )
        .bind(user_id)
        .bind(&hash)
        .bind(&prefix)
        .bind(label)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok((api_key, plaintext))
    }

    /// Authenticate a request by SHA-256 hash of the bearer token.
    /// CRITICAL: filters both revoked_at IS NULL and deleted_at IS NULL.
    pub(crate) async fn authenticate_key_impl(
        &self,
        key_hash: &str,
    ) -> Result<AuthenticatedUser, StoreError> {
        // Row type: (user_id UUID, name TEXT, role TEXT)
        let row: Option<(Uuid, String, UserRole)> = query_as::<_, (Uuid, String, UserRole)>(
            "SELECT u.id, u.name, u.role \
             FROM api_keys k \
             JOIN users u ON u.id = k.user_id \
             WHERE k.key_hash = $1 \
               AND k.revoked_at IS NULL \
               AND u.deleted_at IS NULL",
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        match row {
            Some((user_id, name, role)) => Ok(AuthenticatedUser {
                user_id,
                name,
                role,
            }),
            None => Err(StoreError::KeyNotFound),
        }
    }

    /// List all API keys for a user, ordered by created_at DESC.
    pub(crate) async fn list_keys_for_user_impl(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<ApiKey>, StoreError> {
        let keys = query_as::<_, ApiKey>(
            "SELECT id, user_id, key_hash, key_prefix, label, created_at, revoked_at \
             FROM api_keys WHERE user_id = $1 ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;
        Ok(keys)
    }

    /// Revoke a specific API key by its ID.
    pub(crate) async fn revoke_key_impl(&self, key_id: Uuid) -> Result<(), StoreError> {
        let result = query(
            "UPDATE api_keys SET revoked_at = NOW() WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(key_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            return Err(StoreError::KeyNotFound);
        }

        Ok(())
    }
}
