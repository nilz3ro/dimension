//! SessionStore event methods implementation for PgSessionStore.
//!
//! Provides append_event and get_events for the unified session event log.

use sqlx_core::query_as::query_as;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::{NewSessionEvent, SessionEvent};
use crate::postgres::PgSessionStore;

impl PgSessionStore {
    pub(crate) async fn append_event_impl(
        &self,
        event: NewSessionEvent,
    ) -> Result<SessionEvent, StoreError> {
        let row = query_as::<_, SessionEvent>(
            r#"
            INSERT INTO session_events (session_id, event_type, role, content)
            VALUES ($1, $2, $3, $4)
            RETURNING id, session_id, event_type, role, content, created_at
            "#,
        )
        .bind(event.session_id)
        .bind(event.event_type)
        .bind(&event.role)
        .bind(&event.content)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(row)
    }

    pub(crate) async fn get_events_impl(
        &self,
        session_id: Uuid,
        limit: Option<i64>,
    ) -> Result<Vec<SessionEvent>, StoreError> {
        let effective_limit = limit.unwrap_or(10000);
        let rows = query_as::<_, SessionEvent>(
            r#"
            SELECT id, session_id, event_type, role, content, created_at
            FROM session_events
            WHERE session_id = $1
            ORDER BY created_at ASC, id ASC
            LIMIT $2
            "#,
        )
        .bind(session_id)
        .bind(effective_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(rows)
    }

    pub(crate) async fn get_events_after_impl(
        &self,
        session_id: Uuid,
        after_id: Uuid,
        limit: Option<i64>,
    ) -> Result<Vec<SessionEvent>, StoreError> {
        let effective_limit = limit.unwrap_or(100);
        let rows = query_as::<_, SessionEvent>(
            r#"
            SELECT id, session_id, event_type, role, content, created_at
            FROM session_events
            WHERE session_id = $1 AND id > $2
            ORDER BY created_at ASC, id ASC
            LIMIT $3
            "#,
        )
        .bind(session_id)
        .bind(after_id)
        .bind(effective_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(rows)
    }
}
