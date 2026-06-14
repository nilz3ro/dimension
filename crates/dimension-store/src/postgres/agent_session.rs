//! Agent session store implementation for PgSessionStore.
//!
//! Provides get_or_create_agent_session: persistent caller->target session lookup/creation.

use sqlx_core::query::query;
use sqlx_core::row::Row;
use uuid::Uuid;

use crate::error::StoreError;
use crate::postgres::PgSessionStore;
use crate::store::SessionStore;

impl PgSessionStore {
    /// Get or create a persistent inter-agent session for (caller, target, user) triple.
    ///
    /// Lookup: SELECT session_id FROM agent_sessions WHERE caller=$ AND target=$ AND user_id=$
    /// If found: return existing session_id.
    /// If not found: create a new session via session_store.create_session(), INSERT into
    /// agent_sessions, return new session_id. Uses a transaction for atomicity.
    pub(crate) async fn get_or_create_agent_session_impl(
        &self,
        caller_bundle_id: &str,
        target_bundle_id: &str,
        user_id: Uuid,
        session_store: &dyn SessionStore,
    ) -> Result<Uuid, StoreError> {
        // Fast path: check if a session already exists.
        let existing = query(
            "SELECT session_id FROM agent_sessions
             WHERE caller_bundle_id = $1 AND target_bundle_id = $2 AND user_id = $3",
        )
        .bind(caller_bundle_id)
        .bind(target_bundle_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        if let Some(row) = existing {
            let session_id: Uuid = row.try_get("session_id").map_err(StoreError::from)?;
            return Ok(session_id);
        }

        // No existing session -- create one via the session store.
        let session = session_store
            .create_session(user_id, target_bundle_id)
            .await?;

        let session_id = session.id;

        // Record the binding in agent_sessions.
        // Use ON CONFLICT DO NOTHING to handle races gracefully.
        query(
            "INSERT INTO agent_sessions (caller_bundle_id, target_bundle_id, session_id, user_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (caller_bundle_id, target_bundle_id, user_id) DO NOTHING",
        )
        .bind(caller_bundle_id)
        .bind(target_bundle_id)
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        // Re-read to handle the race where another insert won the conflict.
        let row = query(
            "SELECT session_id FROM agent_sessions
             WHERE caller_bundle_id = $1 AND target_bundle_id = $2 AND user_id = $3",
        )
        .bind(caller_bundle_id)
        .bind(target_bundle_id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;

        let final_session_id: Uuid = row.try_get("session_id").map_err(StoreError::from)?;
        Ok(final_session_id)
    }
}
