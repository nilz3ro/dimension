use sqlx_core::query_as::query_as;

use crate::error::StoreError;
use crate::models::{AuthenticatedUser, UserRole};
use crate::postgres::PgSessionStore;

impl PgSessionStore {
    /// Ensure a bootstrap admin exists. Idempotent — safe to call on every startup.
    ///
    /// - If no users exist: creates an "bootstrap-admin" user with Admin role,
    ///   creates an API key, logs the key prefix, and returns the plaintext key.
    /// - If users already exist: returns None without making any changes.
    pub(crate) async fn ensure_bootstrap_admin_impl(
        &self,
    ) -> Result<Option<String>, StoreError> {
        let count: i64 = query_as::<_, (i64,)>("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::from)?
            .0;

        if count > 0 {
            return Ok(None);
        }

        // No users exist — create the bootstrap admin
        let user = self
            .insert_user_impl("bootstrap-admin", UserRole::Admin)
            .await?;

        let (_, plaintext) = self.create_key_impl(user.id, Some("bootstrap")).await?;

        // Log prefix only — never log the full key
        let prefix = &plaintext[..15];
        tracing::info!(
            user_id = %user.id,
            key_prefix = prefix,
            "bootstrap admin created"
        );

        Ok(Some(plaintext))
    }

    /// Get the bootstrap admin (first admin by created_at). Used for legacy config.token compat.
    pub(crate) async fn get_bootstrap_admin_impl(&self) -> Result<AuthenticatedUser, StoreError> {
        use sqlx_core::query_as::query_as;
        use uuid::Uuid;

        let row: Option<(Uuid, String, UserRole)> = query_as::<_, (Uuid, String, UserRole)>(
            "SELECT id, name, role FROM users \
             WHERE role = 'admin' AND deleted_at IS NULL \
             ORDER BY created_at ASC \
             LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        match row {
            Some((user_id, name, role)) => Ok(AuthenticatedUser {
                user_id,
                name,
                role,
            }),
            None => Err(StoreError::UserNotFound {
                id: uuid::Uuid::nil(),
            }),
        }
    }
}
