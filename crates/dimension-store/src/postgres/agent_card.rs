//! Agent card store implementation for PgSessionStore.
//!
//! Methods: upsert_agent_card, get_agent_card, delete_agent_card.

use sqlx_core::query::query;
use sqlx_core::row::Row;
use uuid::Uuid;

use crate::error::StoreError;
use crate::postgres::PgSessionStore;

impl PgSessionStore {
    /// Insert or update the A2A agent card JSON for a bundle.
    ///
    /// Uses INSERT ON CONFLICT UPDATE so re-uploading a bundle regenerates
    /// the card atomically.
    pub(crate) async fn upsert_agent_card_impl(
        &self,
        bundle_id: &str,
        user_id: Uuid,
        card_json: serde_json::Value,
    ) -> Result<(), StoreError> {
        let card_str = serde_json::to_string(&card_json)
            .map_err(|e| StoreError::Other(format!("failed to serialize card_json: {e}")))?;

        query(
            "INSERT INTO agent_cards (bundle_id, user_id, card_json, updated_at)
             VALUES ($1, $2, $3::jsonb, NOW())
             ON CONFLICT (bundle_id) DO UPDATE
             SET user_id = EXCLUDED.user_id,
                 card_json = EXCLUDED.card_json,
                 updated_at = NOW()",
        )
        .bind(bundle_id)
        .bind(user_id)
        .bind(&card_str)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;

        Ok(())
    }

    /// Get the agent card JSON for a bundle. Returns None if not registered.
    pub(crate) async fn get_agent_card_impl(
        &self,
        bundle_id: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let row = query(
            "SELECT card_json::text FROM agent_cards WHERE bundle_id = $1",
        )
        .bind(bundle_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::from)?;

        match row {
            None => Ok(None),
            Some(r) => {
                let json_str: String = r.try_get("card_json").map_err(StoreError::from)?;
                let value: serde_json::Value = serde_json::from_str(&json_str)
                    .map_err(|e| StoreError::Other(format!("failed to parse card_json: {e}")))?;
                Ok(Some(value))
            }
        }
    }

    /// Delete the agent card for a bundle (called when bundle has no [a2a] section).
    pub(crate) async fn delete_agent_card_impl(
        &self,
        bundle_id: &str,
    ) -> Result<(), StoreError> {
        query("DELETE FROM agent_cards WHERE bundle_id = $1")
            .bind(bundle_id)
            .execute(&self.pool)
            .await
            .map_err(StoreError::from)?;

        Ok(())
    }
}
