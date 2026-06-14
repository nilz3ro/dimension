use sqlx_core::query::query;
use sqlx_core::query_as::query_as;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::{HistoryPage, Message, NewMessage, Session};
use crate::postgres::cursor::{decode_cursor, encode_cursor};
use crate::postgres::PgSessionStore;

impl PgSessionStore {
    /// Append a message to a session's history.
    pub(crate) async fn append_message_impl(
        &self,
        session_id: Uuid,
        msg: NewMessage,
    ) -> Result<Message, StoreError> {
        let role_str = msg.role.to_string();

        let message = query_as::<_, Message>(
            "INSERT INTO messages (session_id, role, content, is_complete) \
             VALUES ($1, $2, $3, $4) \
             RETURNING id, session_id, role, content, is_complete, created_at",
        )
        .bind(session_id)
        .bind(&role_str)
        .bind(&msg.content)
        .bind(msg.is_complete)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        // Touch session updated_at after appending a message
        let _ = query("UPDATE sessions SET updated_at = NOW() WHERE id = $1")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::from)?;

        Ok(message)
    }

    /// Get a paginated page of message history for a session.
    /// Verifies session ownership via user_id before returning messages.
    pub(crate) async fn get_history_impl(
        &self,
        session_id: Uuid,
        user_id: Uuid,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<HistoryPage, StoreError> {
        // Verify session ownership before returning messages
        let session_exists = query_as::<_, Session>(
            "SELECT id, user_id, bundle_id, created_at, updated_at \
             FROM sessions WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL",
        )
        .bind(session_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        if session_exists.is_none() {
            return Err(StoreError::SessionNotFound { id: session_id });
        }
        // Fetch limit+1 rows to determine if there's a next page
        let fetch_limit = (limit as i64) + 1;

        let mut messages: Vec<Message> = if let Some(cursor_str) = cursor {
            let payload = decode_cursor(cursor_str)?;

            query_as::<_, Message>(
                "SELECT id, session_id, role, content, is_complete, created_at \
                 FROM messages \
                 WHERE session_id = $1 \
                   AND (created_at, id) > ($2, $3) \
                 ORDER BY created_at ASC, id ASC \
                 LIMIT $4",
            )
            .bind(session_id)
            .bind(payload.created_at)
            .bind(payload.id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?
        } else {
            query_as::<_, Message>(
                "SELECT id, session_id, role, content, is_complete, created_at \
                 FROM messages \
                 WHERE session_id = $1 \
                 ORDER BY created_at ASC, id ASC \
                 LIMIT $2",
            )
            .bind(session_id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?
        };

        // If we got limit+1 messages, there's another page.
        // Pop the extra sentinel row (not returned to caller), then encode a cursor
        // based on the LAST message of the current page so the next query fetches
        // items strictly after it (WHERE (created_at, id) > cursor).
        let next_cursor = if messages.len() > limit as usize {
            messages.pop(); // discard the sentinel — it would be the first item of the next page
            // The last item we ARE returning is the new page boundary
            let page_tail = messages.last().unwrap();
            Some(encode_cursor(page_tail.created_at, page_tail.id))
        } else {
            None
        };

        Ok(HistoryPage {
            messages,
            next_cursor,
        })
    }
}
