use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx_core::query::query;
use sqlx_core::query_as::query_as;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::{Message, Session, SessionSummary};
use crate::postgres::PgSessionStore;

/// Result of an admin session search — includes session metadata and an
/// optional snippet of the matching message content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSearchResult {
    pub session_id: Uuid,
    pub bundle_id: String,
    pub created_at: DateTime<Utc>,
    /// A short context snippet (~80 chars) centred around the keyword match.
    /// `None` when no keyword was supplied (first message truncated instead).
    pub match_snippet: Option<String>,
}

impl PgSessionStore {
    /// List all sessions without user scoping (admin). Optionally filter by bundle_id.
    pub async fn admin_list_sessions(
        &self,
        bundle_id: Option<&str>,
    ) -> Result<Vec<SessionSummary>, StoreError> {
        use sqlx_core::row::Row;

        let rows = sqlx_core::query::query(
            "SELECT s.id, s.bundle_id, s.created_at, s.updated_at, \
                    COUNT(m.id) as message_count \
             FROM sessions s \
             LEFT JOIN messages m ON m.session_id = s.id \
             WHERE s.deleted_at IS NULL \
               AND ($1::TEXT IS NULL OR s.bundle_id = $1) \
             GROUP BY s.id, s.bundle_id, s.created_at, s.updated_at \
             ORDER BY s.updated_at DESC",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let summaries: Vec<SessionSummary> = rows
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

        Ok(summaries)
    }

    /// Get a session by ID without user scoping (admin).
    pub async fn admin_get_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<Session>, StoreError> {
        let session = query_as::<_, Session>(
            "SELECT id, user_id, bundle_id, created_at, updated_at, metadata \
             FROM sessions \
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(session)
    }

    /// Get full message history for a session without user scoping or pagination (admin).
    pub async fn admin_get_history(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Message>, StoreError> {
        let messages = query_as::<_, Message>(
            "SELECT id, session_id, role, content, is_complete, created_at \
             FROM messages \
             WHERE session_id = $1 \
             ORDER BY created_at ASC, id ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(messages)
    }

    /// Search sessions by keyword, bundle, and/or date range (admin).
    ///
    /// Returns up to `limit` sessions matching ALL supplied filters. For each
    /// result, the `match_snippet` contains ~80 chars centred on the first
    /// keyword match in the earliest matching message.
    ///
    /// Filters:
    /// - `query`  — case-insensitive substring match against message content
    /// - `bundle` — case-insensitive substring match against sessions.bundle_id
    /// - `from`   — sessions created on or after this timestamp
    /// - `to`     — sessions created on or before this timestamp
    pub async fn admin_search_sessions(
        &self,
        query_str: Option<&str>,
        bundle: Option<&str>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Vec<SessionSearchResult>, StoreError> {
        use sqlx_core::row::Row;

        // We use DISTINCT ON (s.id) with ORDER BY s.id, m.created_at ASC so that
        // for each session we get the earliest matching message as the snippet.
        // All filters are optional: NULL parameters are treated as "no filter"
        // by the IS NULL OR ... pattern.
        let rows = sqlx_core::query::query(
            "SELECT DISTINCT ON (s.id) \
                    s.id          AS session_id, \
                    s.bundle_id, \
                    s.created_at, \
                    m.content     AS match_content \
             FROM sessions s \
             LEFT JOIN messages m ON m.session_id = s.id \
             WHERE s.deleted_at IS NULL \
               AND ($1::TEXT IS NULL OR s.bundle_id ILIKE '%' || $1 || '%') \
               AND ($2::TEXT IS NULL OR m.content   ILIKE '%' || $2 || '%') \
               AND ($3::TIMESTAMPTZ IS NULL OR s.created_at >= $3) \
               AND ($4::TIMESTAMPTZ IS NULL OR s.created_at <= $4) \
             ORDER BY s.id, m.created_at ASC \
             LIMIT $5",
        )
        .bind(bundle)
        .bind(query_str)
        .bind(from)
        .bind(to)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let results: Vec<SessionSearchResult> = rows
            .iter()
            .map(|row| {
                let session_id: Uuid = row.try_get("session_id")?;
                let bundle_id: String = row.try_get("bundle_id")?;
                let created_at: DateTime<Utc> = row.try_get("created_at")?;
                let match_content: Option<String> = row.try_get("match_content")?;

                // Build match snippet
                let match_snippet = match_content.as_deref().map(|content| {
                    if let Some(q) = query_str {
                        // Find keyword position (case-insensitive)
                        let lower_content = content.to_lowercase();
                        let lower_q = q.to_lowercase();
                        if let Some(pos) = lower_content.find(&lower_q) {
                            // Extract ~80 chars centred on match
                            let start = pos.saturating_sub(40);
                            let end = (pos + q.len() + 40).min(content.len());
                            let mut snippet = content[start..end].to_string();
                            if start > 0 {
                                snippet = format!("...{snippet}");
                            }
                            if end < content.len() {
                                snippet.push_str("...");
                            }
                            snippet
                        } else {
                            // Keyword not in this message (may match bundle_id only)
                            content.chars().take(80).collect::<String>()
                        }
                    } else {
                        // No keyword — first 80 chars of earliest message
                        content.chars().take(80).collect::<String>()
                    }
                });

                Ok(SessionSearchResult {
                    session_id,
                    bundle_id,
                    created_at,
                    match_snippet,
                })
            })
            .collect::<Result<Vec<_>, sqlx_core::Error>>()
            .map_err(StoreError::from)?;

        Ok(results)
    }

    /// Soft-delete a session without user scoping (admin).
    pub async fn admin_soft_delete_session(
        &self,
        session_id: Uuid,
    ) -> Result<(), StoreError> {
        let result = query(
            "UPDATE sessions SET deleted_at = NOW() \
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        if result.rows_affected() == 0 {
            return Err(StoreError::SessionNotFound { id: session_id });
        }

        Ok(())
    }
}
