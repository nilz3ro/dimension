use sqlx_core::query::query;
use sqlx_core::query_as::query_as;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::{User, UserRole};
use crate::postgres::PgSessionStore;

/// Select clause for User rows -- must match the User FromRow implementation.
const USER_SELECT: &str =
    "id, name, role, created_at, deleted_at, \
     quota_max_sessions, quota_max_bundles, quota_max_concurrent_vms";

impl PgSessionStore {
    /// Create a new user. Called from UserStore::create_user.
    /// Does NOT create an API key — that is the caller's responsibility.
    pub(crate) async fn insert_user_impl(
        &self,
        name: &str,
        role: UserRole,
    ) -> Result<User, StoreError> {
        let sql = format!(
            "INSERT INTO users (name, role) VALUES ($1, $2) \
             RETURNING {USER_SELECT}"
        );
        let user = query_as::<_, User>(&sql)
            .bind(name)
            .bind(role)
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::from)?;

        Ok(user)
    }

    /// Get a user by ID. Returns None if not found or soft-deleted.
    pub(crate) async fn get_user_impl(
        &self,
        user_id: Uuid,
    ) -> Result<Option<User>, StoreError> {
        let sql = format!(
            "SELECT {USER_SELECT} \
             FROM users WHERE id = $1 AND deleted_at IS NULL"
        );
        let user = query_as::<_, User>(&sql)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::from)?;

        Ok(user)
    }

    /// List all active (non-deleted) users ordered by created_at DESC.
    pub(crate) async fn list_users_impl(&self) -> Result<Vec<User>, StoreError> {
        let sql = format!(
            "SELECT {USER_SELECT} \
             FROM users WHERE deleted_at IS NULL \
             ORDER BY created_at DESC"
        );
        let users = query_as::<_, User>(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?;

        Ok(users)
    }

    /// Soft-delete a user. Guards against deleting the last admin.
    pub(crate) async fn soft_delete_user_impl(&self, user_id: Uuid) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StoreError::Connection(e.to_string()))?;

        // Fetch user to check role
        let user_sql = format!(
            "SELECT {USER_SELECT} \
             FROM users WHERE id = $1 AND deleted_at IS NULL"
        );
        let user: Option<User> = query_as::<_, User>(&user_sql)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::from)?;

        let user = user.ok_or(StoreError::UserNotFound { id: user_id })?;

        // Guard: can't delete the last admin
        if user.role == UserRole::Admin {
            let count: i64 = query_as::<_, (i64,)>(
                "SELECT COUNT(*) FROM users WHERE role = 'admin' AND deleted_at IS NULL",
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(StoreError::from)?
            .0;

            if count <= 1 {
                return Err(StoreError::LastAdminDemotion);
            }
        }

        let result = query("UPDATE users SET deleted_at = NOW() WHERE id = $1 AND deleted_at IS NULL")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            return Err(StoreError::UserNotFound { id: user_id });
        }

        tx.commit()
            .await
            .map_err(|e| StoreError::Query(e.to_string()))?;

        Ok(())
    }

    /// Promote a user to admin.
    pub(crate) async fn promote_user_impl(&self, user_id: Uuid) -> Result<(), StoreError> {
        let result = query(
            "UPDATE users SET role = 'admin' WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            return Err(StoreError::UserNotFound { id: user_id });
        }

        Ok(())
    }

    /// Demote a user from admin to regular user. Guards against demoting the last admin.
    pub(crate) async fn demote_user_impl(&self, user_id: Uuid) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StoreError::Connection(e.to_string()))?;

        // Count OTHER active admins (excluding this user)
        let other_admin_count: i64 = query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM users \
             WHERE role = 'admin' AND deleted_at IS NULL AND id != $1",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::from)?
        .0;

        if other_admin_count < 1 {
            return Err(StoreError::LastAdminDemotion);
        }

        let result = query(
            "UPDATE users SET role = 'user' WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            return Err(StoreError::UserNotFound { id: user_id });
        }

        tx.commit()
            .await
            .map_err(|e| StoreError::Query(e.to_string()))?;

        Ok(())
    }

    /// Count active admins.
    pub(crate) async fn admin_count_impl(&self) -> Result<i64, StoreError> {
        let count: i64 = query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM users WHERE role = 'admin' AND deleted_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?
        .0;

        Ok(count)
    }

}
