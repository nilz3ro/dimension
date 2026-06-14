pub mod cursor;
pub mod admin;
mod message;
mod session;
mod user;
mod api_key;
mod bootstrap;
mod secrets;
mod storage;
mod agent_card;
mod agent_session;
mod task;
mod event;
mod volume;
mod artifact;
pub mod deployment;
mod named_volume;

use sqlx_core::migrate::{Migration, MigrationType, Migrator};
use sqlx_postgres::{PgPool, PgPoolOptions};
use std::borrow::Cow;
use async_trait::async_trait;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::{ApiKey, AuthenticatedUser, HistoryPage, Message, NewMessage, SecretMetadata, Session, SessionSummary, TokenRecord, User, UserRole};
use crate::store::{SecretStore, SessionStore, UserStore};

// Embed migration SQL at compile time to avoid runtime path resolution issues.
// This approach replaces the migrate! macro (which requires sqlx-macros, unavailable
// due to the sqlx-core/sqlx-postgres split to avoid libsqlite3-sys conflict).
const MIGRATION_0001_SQL: &str = include_str!("../../migrations/0001_create_sessions.sql");
const MIGRATION_0002_SQL: &str = include_str!("../../migrations/0002_create_messages.sql");
const MIGRATION_0003_SQL: &str = include_str!("../../migrations/0003_create_users.sql");
const MIGRATION_0004_SQL: &str = include_str!("../../migrations/0004_create_api_keys.sql");
const MIGRATION_0005_SQL: &str = include_str!("../../migrations/0005_session_management.sql");
const MIGRATION_0006_SQL: &str = include_str!("../../migrations/0006_create_secrets_tokens.sql");
const MIGRATION_0007_SQL: &str = include_str!("../../migrations/0007_create_bundle_storage.sql");
const MIGRATION_0008_SQL: &str = include_str!("../../migrations/0008_a2a_tasks.sql");
const MIGRATION_0009_SQL: &str = include_str!("../../migrations/0009_pulsar_subscriptions.sql");
const MIGRATION_0010_SQL: &str = include_str!("../../migrations/0010_create_volumes.sql");
const MIGRATION_0011_SQL: &str = include_str!("../../migrations/0011_create_artifacts.sql");
const MIGRATION_0012_SQL: &str = include_str!("../../migrations/0012_create_session_events.sql");
const MIGRATION_0013_SQL: &str = include_str!("../../migrations/0013_volumes_last_accessed.sql");
const MIGRATION_0014_SQL: &str = include_str!("../../migrations/0014_create_deployments.sql");
const MIGRATION_0015_SQL: &str = include_str!("../../migrations/0015_artifacts_ttl.sql");
const MIGRATION_0016_SQL: &str = include_str!("../../migrations/0016_named_volumes.sql");
const MIGRATION_0017_SQL: &str = include_str!("../../migrations/0017_subscription_consumer_count.sql");
const MIGRATION_0018_SQL: &str = include_str!("../../migrations/0018_session_webhooks_metadata.sql");
const MIGRATION_0019_SQL: &str = include_str!("../../migrations/0019_drop_session_webhook_url.sql");

/// Build the Migrator from embedded SQL constants.
fn build_migrator() -> Migrator {
    let migrations = vec![
        Migration::new(
            1,
            Cow::Borrowed("create_sessions"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0001_SQL),
            false,
        ),
        Migration::new(
            2,
            Cow::Borrowed("create_messages"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0002_SQL),
            false,
        ),
        Migration::new(
            3,
            Cow::Borrowed("create_users"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0003_SQL),
            false,
        ),
        Migration::new(
            4,
            Cow::Borrowed("create_api_keys"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0004_SQL),
            false,
        ),
        Migration::new(
            5,
            Cow::Borrowed("session_management"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0005_SQL),
            false,
        ),
        Migration::new(
            6,
            Cow::Borrowed("create_secrets_tokens"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0006_SQL),
            false,
        ),
        Migration::new(
            7,
            Cow::Borrowed("create_bundle_storage"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0007_SQL),
            false,
        ),
        Migration::new(
            8,
            Cow::Borrowed("create a2a tasks"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0008_SQL),
            false,
        ),
        Migration::new(
            9,
            Cow::Borrowed("create pulsar subscriptions"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0009_SQL),
            false,
        ),
        Migration::new(
            10,
            Cow::Borrowed("create_volumes"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0010_SQL),
            false,
        ),
        Migration::new(
            11,
            Cow::Borrowed("create_artifacts"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0011_SQL),
            false,
        ),
        Migration::new(
            12,
            Cow::Borrowed("create_session_events"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0012_SQL),
            false,
        ),
        Migration::new(
            13,
            Cow::Borrowed("volumes_last_accessed"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0013_SQL),
            false,
        ),
        Migration::new(
            14,
            Cow::Borrowed("create_deployments"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0014_SQL),
            false,
        ),
        Migration::new(
            15,
            Cow::Borrowed("artifacts_ttl"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0015_SQL),
            false,
        ),
        Migration::new(
            16,
            Cow::Borrowed("create_named_volumes"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0016_SQL),
            false,
        ),
        Migration::new(
            17,
            Cow::Borrowed("subscription_consumer_count"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0017_SQL),
            false,
        ),
        Migration::new(
            18,
            Cow::Borrowed("session_webhooks_metadata"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0018_SQL),
            false,
        ),
        Migration::new(
            19,
            Cow::Borrowed("drop_session_webhook_url"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0019_SQL),
            false,
        ),
    ];
    Migrator {
        migrations: Cow::Owned(migrations),
        ..Migrator::DEFAULT
    }
}

/// Postgres-backed implementation of [`SessionStore`].
///
/// Uses a connection pool and runs migrations automatically on [`connect()`][PgSessionStore::connect].
pub struct PgSessionStore {
    pub(crate) pool: PgPool,
}

impl PgSessionStore {
    /// Expose the underlying connection pool (primarily for test helpers).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Connect to the Postgres database at `database_url` and run pending migrations.
    ///
    /// Creates a pool with max 5 connections, min 1, and a 5-second acquire timeout.
    /// Migrations are embedded at compile time and run automatically.
    pub async fn connect(database_url: &str) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .min_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(database_url)
            .await
            .map_err(|e| StoreError::Connection(e.to_string()))?;

        build_migrator()
            .run(&pool)
            .await
            .map_err(|e| StoreError::Connection(format!("migration run error: {e}")))?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn create_session(&self, user_id: Uuid, bundle_id: &str) -> Result<Session, StoreError> {
        self.create_session_impl(user_id, bundle_id).await
    }

    async fn create_session_with_id(&self, id: Uuid, user_id: Uuid, bundle_id: &str) -> Result<Session, StoreError> {
        self.create_session_with_id_impl(id, user_id, bundle_id).await
    }

    async fn get_session(&self, session_id: Uuid, user_id: Uuid) -> Result<Option<Session>, StoreError> {
        self.get_session_impl(session_id, user_id).await
    }

    async fn append_message(
        &self,
        session_id: Uuid,
        msg: NewMessage,
    ) -> Result<Message, StoreError> {
        self.append_message_impl(session_id, msg).await
    }

    async fn get_history(
        &self,
        session_id: Uuid,
        user_id: Uuid,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<HistoryPage, StoreError> {
        self.get_history_impl(session_id, user_id, cursor, limit).await
    }

    async fn touch(&self, session_id: Uuid) -> Result<(), StoreError> {
        self.touch_impl(session_id).await
    }

    async fn delete_session(&self, session_id: Uuid) -> Result<(), StoreError> {
        self.delete_session_impl(session_id).await
    }

    async fn soft_delete_session(&self, session_id: Uuid, user_id: Uuid) -> Result<(), StoreError> {
        self.soft_delete_session_impl(session_id, user_id).await
    }

    async fn list_sessions(
        &self,
        user_id: Uuid,
        bundle_id: Option<&str>,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<SessionSummary>, Option<String>), StoreError> {
        self.list_sessions_impl(user_id, bundle_id, cursor, limit).await
    }

    async fn count_active_sessions(&self, user_id: Uuid) -> Result<i64, StoreError> {
        self.count_active_sessions_impl(user_id).await
    }

    async fn count_session_messages(&self, session_id: Uuid) -> Result<i64, StoreError> {
        self.count_session_messages_impl(session_id).await
    }

    async fn append_event(&self, event: crate::models::NewSessionEvent) -> Result<crate::models::SessionEvent, StoreError> {
        self.append_event_impl(event).await
    }

    async fn get_events(&self, session_id: Uuid, limit: Option<i64>) -> Result<Vec<crate::models::SessionEvent>, StoreError> {
        self.get_events_impl(session_id, limit).await
    }

    async fn get_events_after(&self, session_id: Uuid, after_id: Uuid, limit: Option<i64>) -> Result<Vec<crate::models::SessionEvent>, StoreError> {
        self.get_events_after_impl(session_id, after_id, limit).await
    }


    async fn get_metadata(&self, session_id: Uuid) -> Result<serde_json::Value, StoreError> {
        self.get_metadata_impl(session_id).await
    }

    async fn update_metadata(&self, session_id: Uuid, patch: serde_json::Value) -> Result<serde_json::Value, StoreError> {
        self.update_metadata_impl(session_id, patch).await
    }

    async fn admin_list_sessions(&self, bundle_id: Option<&str>) -> Result<Vec<SessionSummary>, StoreError> {
        self.admin_list_sessions(bundle_id).await
    }

    async fn admin_get_session(&self, session_id: Uuid) -> Result<Option<Session>, StoreError> {
        self.admin_get_session(session_id).await
    }

    async fn admin_get_history(&self, session_id: Uuid) -> Result<Vec<Message>, StoreError> {
        self.admin_get_history(session_id).await
    }

    async fn admin_search_sessions(
        &self,
        query: Option<&str>,
        bundle: Option<&str>,
        from: Option<chrono::DateTime<chrono::Utc>>,
        to: Option<chrono::DateTime<chrono::Utc>>,
        limit: i64,
    ) -> Result<Vec<crate::postgres::admin::SessionSearchResult>, StoreError> {
        self.admin_search_sessions(query, bundle, from, to, limit).await
    }

    async fn admin_soft_delete_session(&self, session_id: Uuid) -> Result<(), StoreError> {
        self.admin_soft_delete_session(session_id).await
    }
}

#[async_trait]
impl UserStore for PgSessionStore {
    async fn create_user(&self, name: &str, role: UserRole) -> Result<(User, String), StoreError> {
        let user = self.insert_user_impl(name, role).await?;
        let (api_key, plaintext) = self.create_key_impl(user.id, None).await?;
        let _ = api_key; // metadata not needed at creation time
        Ok((user, plaintext))
    }

    async fn get_user(&self, user_id: Uuid) -> Result<Option<User>, StoreError> {
        self.get_user_impl(user_id).await
    }

    async fn list_users(&self) -> Result<Vec<User>, StoreError> {
        self.list_users_impl().await
    }

    async fn soft_delete_user(&self, user_id: Uuid) -> Result<(), StoreError> {
        self.soft_delete_user_impl(user_id).await
    }

    async fn promote_user(&self, user_id: Uuid) -> Result<(), StoreError> {
        self.promote_user_impl(user_id).await
    }

    async fn demote_user(&self, user_id: Uuid) -> Result<(), StoreError> {
        self.demote_user_impl(user_id).await
    }

    async fn admin_count(&self) -> Result<i64, StoreError> {
        self.admin_count_impl().await
    }

    async fn create_key(&self, user_id: Uuid, label: Option<&str>) -> Result<(ApiKey, String), StoreError> {
        self.create_key_impl(user_id, label).await
    }

    async fn authenticate_key(&self, key_hash: &str) -> Result<AuthenticatedUser, StoreError> {
        self.authenticate_key_impl(key_hash).await
    }

    async fn revoke_key(&self, key_id: Uuid) -> Result<(), StoreError> {
        self.revoke_key_impl(key_id).await
    }

    async fn list_keys_for_user(&self, user_id: Uuid) -> Result<Vec<ApiKey>, StoreError> {
        self.list_keys_for_user_impl(user_id).await
    }

    async fn ensure_bootstrap_admin(&self) -> Result<Option<String>, StoreError> {
        self.ensure_bootstrap_admin_impl().await
    }

    async fn get_bootstrap_admin(&self) -> Result<AuthenticatedUser, StoreError> {
        self.get_bootstrap_admin_impl().await
    }
}

#[async_trait]
impl SecretStore for PgSessionStore {
    async fn upsert_secret_metadata(
        &self,
        user_id: Uuid,
        bundle_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        self.upsert_secret_metadata_impl(user_id, bundle_id, name).await
    }

    async fn list_secret_metadata(
        &self,
        user_id: Uuid,
        bundle_id: &str,
    ) -> Result<Vec<SecretMetadata>, StoreError> {
        self.list_secret_metadata_impl(user_id, bundle_id).await
    }

    async fn delete_secret_metadata(
        &self,
        user_id: Uuid,
        bundle_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        self.delete_secret_metadata_impl(user_id, bundle_id, name).await
    }

    async fn insert_token(
        &self,
        id: &str,
        bundle_id: &str,
        user_id: Uuid,
        ciphertext: &str,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), StoreError> {
        self.insert_token_impl(id, bundle_id, user_id, ciphertext, expires_at).await
    }

    async fn get_token(
        &self,
        id: &str,
        bundle_id: &str,
    ) -> Result<Option<TokenRecord>, StoreError> {
        self.get_token_impl(id, bundle_id).await
    }

    async fn delete_expired_tokens(&self) -> Result<u64, StoreError> {
        self.delete_expired_tokens_impl().await
    }
}
