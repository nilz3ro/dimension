use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use sqlx_core::query::query;
use sqlx_core::query_as::query_as;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::{Session, SessionSummary};
use crate::postgres::PgSessionStore;

impl PgSessionStore {
    /// Create a new session for the given user and bundle.
    pub(crate) async fn create_session_impl(
        &self,
        user_id: Uuid,
        bundle_id: &str,
    ) -> Result<Session, StoreError> {
        let session = query_as::<_, Session>(
            "INSERT INTO sessions (user_id, bundle_id) VALUES ($1, $2) \
             RETURNING id, user_id, bundle_id, created_at, updated_at, metadata",
        )
        .bind(user_id)
        .bind(bundle_id)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(session)
    }

    /// Create a session with a caller-supplied UUID.
    pub(crate) async fn create_session_with_id_impl(
        &self,
        id: Uuid,
        user_id: Uuid,
        bundle_id: &str,
    ) -> Result<Session, StoreError> {
        let session = query_as::<_, Session>(
            "INSERT INTO sessions (id, user_id, bundle_id) VALUES ($1, $2, $3) \
             RETURNING id, user_id, bundle_id, created_at, updated_at, metadata",
        )
        .bind(id)
        .bind(user_id)
        .bind(bundle_id)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(session)
    }

    /// Get a session by its ID, scoped to user_id.
    /// Excludes soft-deleted sessions (deleted_at IS NOT NULL).
    pub(crate) async fn get_session_impl(
        &self,
        session_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Session>, StoreError> {
        let session = query_as::<_, Session>(
            "SELECT id, user_id, bundle_id, created_at, updated_at, metadata \
             FROM sessions \
             WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL",
        )
        .bind(session_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(session)
    }

    /// Update the session's updated_at timestamp to NOW().
    pub(crate) async fn touch_impl(&self, session_id: Uuid) -> Result<(), StoreError> {
        let result = query("UPDATE sessions SET updated_at = NOW() WHERE id = $1")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            return Err(StoreError::SessionNotFound { id: session_id });
        }

        Ok(())
    }

    /// Hard-delete a session and cascade to its messages.
    pub(crate) async fn delete_session_impl(&self, session_id: Uuid) -> Result<(), StoreError> {
        let result = query("DELETE FROM sessions WHERE id = $1")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            return Err(StoreError::SessionNotFound { id: session_id });
        }

        Ok(())
    }

    /// Soft-delete a session (sets deleted_at). Scoped by user_id.
    pub(crate) async fn soft_delete_session_impl(
        &self,
        session_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), StoreError> {
        let result = query(
            "UPDATE sessions SET deleted_at = NOW() \
             WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL",
        )
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            return Err(StoreError::SessionNotFound { id: session_id });
        }

        Ok(())
    }

    /// List sessions for a user, optionally filtered by bundle_id.
    /// Uses cursor-based pagination (cursor = base64-encoded updated_at timestamp).
    /// Soft-deleted sessions are excluded.
    pub(crate) async fn list_sessions_impl(
        &self,
        user_id: Uuid,
        bundle_id: Option<&str>,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<SessionSummary>, Option<String>), StoreError> {
        // Decode cursor to get the updated_at cutoff timestamp
        let cursor_ts: Option<DateTime<Utc>> = if let Some(c) = cursor {
            let bytes = URL_SAFE_NO_PAD
                .decode(c)
                .map_err(|_| StoreError::InvalidCursor { reason: "base64 decode failed".into() })?;
            let ts_str = std::str::from_utf8(&bytes)
                .map_err(|_| StoreError::InvalidCursor { reason: "invalid UTF-8".into() })?;
            let ts = ts_str
                .parse::<DateTime<Utc>>()
                .map_err(|_| StoreError::InvalidCursor { reason: "invalid timestamp".into() })?;
            Some(ts)
        } else {
            None
        };

        let fetch_limit = limit + 1;

        // We use a raw query approach since sqlx runtime queries don't support optional filters
        // well without dynamic SQL. We use conditional IS NULL checks on the bundle_id parameter.
        let rows = if let Some(ts) = cursor_ts {
            sqlx_core::query::query(
                "SELECT s.id, s.bundle_id, s.created_at, s.updated_at, \
                        COUNT(m.id) as message_count \
                 FROM sessions s \
                 LEFT JOIN messages m ON m.session_id = s.id \
                 WHERE s.user_id = $1 AND s.deleted_at IS NULL \
                   AND ($2::TEXT IS NULL OR s.bundle_id = $2) \
                   AND s.updated_at < $3 \
                 GROUP BY s.id, s.bundle_id, s.created_at, s.updated_at \
                 ORDER BY s.updated_at DESC \
                 LIMIT $4",
            )
            .bind(user_id)
            .bind(bundle_id)
            .bind(ts)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?
        } else {
            sqlx_core::query::query(
                "SELECT s.id, s.bundle_id, s.created_at, s.updated_at, \
                        COUNT(m.id) as message_count \
                 FROM sessions s \
                 LEFT JOIN messages m ON m.session_id = s.id \
                 WHERE s.user_id = $1 AND s.deleted_at IS NULL \
                   AND ($2::TEXT IS NULL OR s.bundle_id = $2) \
                 GROUP BY s.id, s.bundle_id, s.created_at, s.updated_at \
                 ORDER BY s.updated_at DESC \
                 LIMIT $3",
            )
            .bind(user_id)
            .bind(bundle_id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?
        };

        use sqlx_core::row::Row;

        let mut summaries: Vec<SessionSummary> = rows
            .iter()
            .map(|row| {
                Ok(SessionSummary {
                    id: row.try_get("id")?,
                    bundle_id: row.try_get("bundle_id")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                    message_count: row.try_get("message_count")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx_core::Error>>()
            .map_err(StoreError::from)?;

        // Check if there is a next page
        let next_cursor = if summaries.len() > limit as usize {
            summaries.pop(); // discard sentinel
            let last = summaries.last().unwrap();
            let ts_str = last.updated_at.to_rfc3339();
            Some(URL_SAFE_NO_PAD.encode(ts_str.as_bytes()))
        } else {
            None
        };

        Ok((summaries, next_cursor))
    }

    /// Count active (non-deleted) sessions for a user.
    pub(crate) async fn count_active_sessions_impl(
        &self,
        user_id: Uuid,
    ) -> Result<i64, StoreError> {
        let row = sqlx_core::query::query(
            "SELECT COUNT(*) as count FROM sessions WHERE user_id = $1 AND deleted_at IS NULL",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        use sqlx_core::row::Row;
        let count: i64 = row.try_get("count").map_err(StoreError::from)?;
        Ok(count)
    }

    /// Count total messages in a session.
    pub(crate) async fn count_session_messages_impl(
        &self,
        session_id: Uuid,
    ) -> Result<i64, StoreError> {
        let row = sqlx_core::query::query(
            "SELECT COUNT(*) as count FROM messages WHERE session_id = $1",
        )
        .bind(session_id)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        use sqlx_core::row::Row;
        let count: i64 = row.try_get("count").map_err(StoreError::from)?;
        Ok(count)
    }

    /// Get metadata for a session.
    pub(crate) async fn get_metadata_impl(
        &self,
        session_id: Uuid,
    ) -> Result<serde_json::Value, StoreError> {
        let row = sqlx_core::query::query(
            "SELECT metadata FROM sessions WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        use sqlx_core::row::Row;
        match row {
            Some(r) => {
                let metadata: serde_json::Value = r.try_get("metadata").map_err(StoreError::from)?;
                Ok(metadata)
            }
            None => Err(StoreError::SessionNotFound { id: session_id }),
        }
    }

    /// Merge-update metadata using Postgres JSONB || operator.
    /// Returns the updated full metadata object.
    pub(crate) async fn update_metadata_impl(
        &self,
        session_id: Uuid,
        patch: serde_json::Value,
    ) -> Result<serde_json::Value, StoreError> {
        let row = sqlx_core::query::query(
            "UPDATE sessions SET metadata = metadata || $2, updated_at = NOW() \
             WHERE id = $1 AND deleted_at IS NULL \
             RETURNING metadata",
        )
        .bind(session_id)
        .bind(patch)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        use sqlx_core::row::Row;
        match row {
            Some(r) => {
                let metadata: serde_json::Value = r.try_get("metadata").map_err(StoreError::from)?;
                Ok(metadata)
            }
            None => Err(StoreError::SessionNotFound { id: session_id }),
        }
    }
}
