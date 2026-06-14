use async_trait::async_trait;
use bytes::Bytes;
use opendal::Operator;
use uuid::Uuid;

use crate::error::StoreError;
use crate::models::{ApiKey, Artifact, AuthenticatedUser, BundleStorageRecord, Deployment, HistoryPage, Message, NamedVolume, NewDeployment, NewMessage, NewSessionEvent, NewTask, NewTaskRun, SecretMetadata, Session, SessionEvent, SessionSummary, Task, TaskRun, TokenRecord, User, UserRole, Volume};
use crate::postgres::admin::SessionSearchResult;

#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Create a new session for the given user and bundle.
    async fn create_session(&self, user_id: Uuid, bundle_id: &str) -> Result<Session, StoreError>;

    /// Create a session with a pre-assigned ID.
    ///
    /// Used by the messages handler to ensure the session UUID returned in the
    /// 202 response matches the real session. Falls back to [`create_session`]
    /// if the implementation doesn't support caller-supplied IDs.
    async fn create_session_with_id(&self, id: Uuid, user_id: Uuid, bundle_id: &str) -> Result<Session, StoreError> {
        // Default: ignore the supplied ID and let the store generate one.
        // Implementations should override this.
        let _ = id;
        self.create_session(user_id, bundle_id).await
    }

    /// Get a session by ID, scoped to the given user_id.
    /// Returns None if the session does not exist or belongs to a different user.
    /// Soft-deleted sessions (deleted_at IS NOT NULL) are also excluded.
    async fn get_session(&self, session_id: Uuid, user_id: Uuid) -> Result<Option<Session>, StoreError>;

    async fn append_message(
        &self,
        session_id: Uuid,
        msg: NewMessage,
    ) -> Result<Message, StoreError>;

    /// Get paginated history for a session. Scoped to user_id for isolation.
    async fn get_history(
        &self,
        session_id: Uuid,
        user_id: Uuid,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<HistoryPage, StoreError>;

    async fn touch(&self, session_id: Uuid) -> Result<(), StoreError>;

    /// Hard-delete a session (for internal use and backward compat).
    async fn delete_session(&self, session_id: Uuid) -> Result<(), StoreError>;

    /// Soft-delete a session (sets deleted_at). Scoped by user_id.
    /// Returns SessionNotFound if session does not exist or belongs to another user.
    async fn soft_delete_session(&self, session_id: Uuid, user_id: Uuid) -> Result<(), StoreError>;

    /// List sessions for a user, optionally filtered by bundle_id.
    /// Returns cursor-based paginated results sorted by updated_at DESC.
    /// Soft-deleted sessions (deleted_at IS NOT NULL) are excluded.
    async fn list_sessions(
        &self,
        user_id: Uuid,
        bundle_id: Option<&str>,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<SessionSummary>, Option<String>), StoreError>;

    /// Count active (non-deleted) sessions for a user. Used for quota enforcement.
    async fn count_active_sessions(&self, user_id: Uuid) -> Result<i64, StoreError>;

    /// Count total messages in a session.
    async fn count_session_messages(&self, session_id: Uuid) -> Result<i64, StoreError>;

    /// Append a unified event to the session_events log.
    async fn append_event(&self, event: NewSessionEvent) -> Result<SessionEvent, StoreError>;

    /// Get session events in chronological order.
    /// If limit is None, returns up to 10000 events.
    async fn get_events(&self, session_id: Uuid, limit: Option<i64>) -> Result<Vec<SessionEvent>, StoreError>;

    /// Get session events after a cursor (event ID) for polling.
    async fn get_events_after(&self, session_id: Uuid, after_id: Uuid, limit: Option<i64>) -> Result<Vec<SessionEvent>, StoreError>;

    /// Get metadata for a session.
    async fn get_metadata(&self, session_id: Uuid) -> Result<serde_json::Value, StoreError>;

    /// Merge-update metadata for a session (JSON merge patch semantics via Postgres ||).
    /// Returns the updated full metadata object.
    async fn update_metadata(&self, session_id: Uuid, patch: serde_json::Value) -> Result<serde_json::Value, StoreError>;

    // ── Admin methods (cross-user, no ownership scoping) ─────────────────────

    /// Admin: list all sessions without user scoping. Optional bundle filter.
    async fn admin_list_sessions(&self, bundle_id: Option<&str>) -> Result<Vec<SessionSummary>, StoreError>;

    /// Admin: get session by ID without user scoping.
    async fn admin_get_session(&self, session_id: Uuid) -> Result<Option<Session>, StoreError>;

    /// Admin: get full message history without user scoping.
    async fn admin_get_history(&self, session_id: Uuid) -> Result<Vec<Message>, StoreError>;

    /// Admin: search sessions by keyword/bundle/date range.
    async fn admin_search_sessions(
        &self,
        query: Option<&str>,
        bundle: Option<&str>,
        from: Option<chrono::DateTime<chrono::Utc>>,
        to: Option<chrono::DateTime<chrono::Utc>>,
        limit: i64,
    ) -> Result<Vec<SessionSearchResult>, StoreError>;

    /// Admin: soft-delete any session without user scoping.
    async fn admin_soft_delete_session(&self, session_id: Uuid) -> Result<(), StoreError>;
}

/// Store trait for user identity and API key management.
#[async_trait]
pub trait UserStore: Send + Sync {
    /// Create a user with a name and role. Returns the User and a plaintext API key.
    /// The plaintext key is returned ONCE and never stored.
    async fn create_user(&self, name: &str, role: UserRole) -> Result<(User, String), StoreError>;

    /// Get a user by ID (returns None if not found or soft-deleted).
    async fn get_user(&self, user_id: Uuid) -> Result<Option<User>, StoreError>;

    /// List all active (non-deleted) users.
    async fn list_users(&self) -> Result<Vec<User>, StoreError>;

    /// Soft-delete a user (sets deleted_at). Guards against deleting the last admin.
    async fn soft_delete_user(&self, user_id: Uuid) -> Result<(), StoreError>;

    /// Promote a user to admin role.
    async fn promote_user(&self, user_id: Uuid) -> Result<(), StoreError>;

    /// Demote a user from admin to regular user. Guards against demoting the last admin.
    async fn demote_user(&self, user_id: Uuid) -> Result<(), StoreError>;

    /// Count active admins.
    async fn admin_count(&self) -> Result<i64, StoreError>;

    /// Create an additional API key for a user. Returns (ApiKey metadata, plaintext key).
    async fn create_key(&self, user_id: Uuid, label: Option<&str>) -> Result<(ApiKey, String), StoreError>;

    /// Authenticate a request by SHA-256 hash of the bearer token.
    /// Returns the resolved user identity, or error if key invalid/revoked/user deleted.
    async fn authenticate_key(&self, key_hash: &str) -> Result<AuthenticatedUser, StoreError>;

    /// Revoke a specific API key by its ID.
    async fn revoke_key(&self, key_id: Uuid) -> Result<(), StoreError>;

    /// List all API keys for a user (including revoked ones), ordered by created_at DESC.
    async fn list_keys_for_user(&self, user_id: Uuid) -> Result<Vec<ApiKey>, StoreError>;

    /// Ensure a bootstrap admin exists. Called at startup.
    /// If no users exist, creates an admin user and returns its plaintext API key.
    /// If users already exist, returns None.
    async fn ensure_bootstrap_admin(&self) -> Result<Option<String>, StoreError>;

    /// Get the bootstrap admin user (for legacy config.token backward compatibility).
    async fn get_bootstrap_admin(&self) -> Result<AuthenticatedUser, StoreError>;
}

/// Store trait for secret metadata and tokenization records.
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Insert or update secret metadata for (user_id, bundle_id, name).
    async fn upsert_secret_metadata(
        &self,
        user_id: Uuid,
        bundle_id: &str,
        name: &str,
    ) -> Result<(), StoreError>;

    /// List all secret metadata for a bundle, sorted by name.
    async fn list_secret_metadata(
        &self,
        user_id: Uuid,
        bundle_id: &str,
    ) -> Result<Vec<SecretMetadata>, StoreError>;

    /// Delete secret metadata for (user_id, bundle_id, name).
    async fn delete_secret_metadata(
        &self,
        user_id: Uuid,
        bundle_id: &str,
        name: &str,
    ) -> Result<(), StoreError>;

    /// Insert a tokenization record.
    async fn insert_token(
        &self,
        id: &str,
        bundle_id: &str,
        user_id: Uuid,
        ciphertext: &str,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), StoreError>;

    /// Get a token record by id, scoped to bundle_id (cross-bundle isolation).
    async fn get_token(
        &self,
        id: &str,
        bundle_id: &str,
    ) -> Result<Option<TokenRecord>, StoreError>;

    /// Delete all expired tokens (where expires_at < now).
    /// Returns the number of deleted rows.
    async fn delete_expired_tokens(&self) -> Result<u64, StoreError>;
}

/// Store trait for bundle object storage quota tracking.
#[async_trait]
pub trait StorageStore: Send + Sync {
    /// Get storage info (bytes_used, quota_bytes) for a bundle.
    /// Creates the record with defaults if it does not yet exist.
    async fn get_storage_info(&self, user_id: Uuid, bundle_id: &str) -> Result<(i64, i64), StoreError>;

    /// Increment bytes_used by delta for the given bundle.
    /// Returns the new bytes_used value after the increment.
    async fn increment_bytes_used(&self, user_id: Uuid, bundle_id: &str, delta: i64) -> Result<i64, StoreError>;

    /// Decrement bytes_used by delta for the given bundle (floors at zero).
    async fn decrement_bytes_used(&self, user_id: Uuid, bundle_id: &str, delta: i64) -> Result<(), StoreError>;

    /// Set the storage quota for a bundle.
    async fn set_quota(&self, user_id: Uuid, bundle_id: &str, quota_bytes: i64) -> Result<(), StoreError>;

    /// List all storage records, ordered by updated_at DESC.
    async fn list_storage_stats(&self) -> Result<Vec<BundleStorageRecord>, StoreError>;

    /// Set bytes_used to an exact value (for reconciliation after sync).
    async fn set_bytes_used(&self, user_id: Uuid, bundle_id: &str, bytes: i64) -> Result<(), StoreError>;
}

/// Store trait for A2A agent cards, inter-agent sessions, and task orchestration.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Insert or update the agent card JSON for a bundle.
    async fn upsert_agent_card(
        &self,
        bundle_id: &str,
        user_id: Uuid,
        card_json: serde_json::Value,
    ) -> Result<(), StoreError>;

    /// Get the agent card JSON for a bundle. Returns None if not found.
    async fn get_agent_card(
        &self,
        bundle_id: &str,
    ) -> Result<Option<serde_json::Value>, StoreError>;

    /// Delete the agent card for a bundle.
    async fn delete_agent_card(&self, bundle_id: &str) -> Result<(), StoreError>;

    /// Get or create a persistent inter-agent session for caller -> target pair.
    ///
    /// Looks up an existing agent_sessions row. If none exists, creates a new
    /// session via the session store and records it in agent_sessions.
    /// Returns the session_id.
    async fn get_or_create_agent_session(
        &self,
        caller_bundle_id: &str,
        target_bundle_id: &str,
        user_id: Uuid,
        session_store: &dyn SessionStore,
    ) -> Result<Uuid, StoreError>;

    /// Create a new task. Inserts the task and any target_bundle_ids.
    async fn create_task(&self, task: NewTask) -> Result<Task, StoreError>;

    /// Get a task by ID, scoped to user_id. Returns None if not found.
    async fn get_task(&self, task_id: Uuid, user_id: Uuid) -> Result<Option<Task>, StoreError>;

    /// Get a task by ID, scoped to both user_id and bundle_id.
    /// Returns None if the task does not belong to this user AND bundle pair.
    /// Prevents cross-bundle task leakage in the A2A endpoint.
    async fn get_task_scoped(
        &self,
        task_id: Uuid,
        user_id: Uuid,
        bundle_id: &str,
    ) -> Result<Option<Task>, StoreError>;

    /// List tasks for a user with optional status filter and cursor-based pagination.
    /// Returns (tasks, next_cursor).
    async fn list_tasks(
        &self,
        user_id: Uuid,
        status: Option<&str>,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Task>, Option<String>), StoreError>;

    /// Update the status of a task.
    async fn update_task_status(&self, task_id: Uuid, status: &str) -> Result<(), StoreError>;

    /// Cancel a task. Only transitions from pending or running to cancelled.
    async fn cancel_task(&self, task_id: Uuid, user_id: Uuid) -> Result<(), StoreError>;

    /// Claim tasks that are ready to run using FOR UPDATE SKIP LOCKED.
    async fn claim_ready_tasks(&self, limit: i32) -> Result<Vec<Task>, StoreError>;

    /// Record a completed task iteration: inserts a task_run and updates task fields.
    async fn complete_task_iteration(
        &self,
        task_id: Uuid,
        run: NewTaskRun,
    ) -> Result<(), StoreError>;

    /// Get all task run records for a task, scoped to user_id.
    async fn get_task_runs(
        &self,
        task_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<TaskRun>, StoreError>;

    /// Count running tasks for a user.
    async fn count_running_tasks(&self, user_id: Uuid) -> Result<i64, StoreError>;

    /// Get all target bundle_ids for a task (fan-out support).
    async fn get_task_targets(&self, task_id: Uuid) -> Result<Vec<String>, StoreError>;

    /// Set the next scheduled run time for a task.
    async fn set_task_next_run(
        &self,
        task_id: Uuid,
        next_run_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), StoreError>;

    /// Admin: list all tasks across all users with optional status filter.
    async fn admin_list_tasks(
        &self,
        status: Option<&str>,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Task>, Option<String>), StoreError>;

    /// Admin: get task by ID without user scoping.
    async fn admin_get_task(&self, task_id: Uuid) -> Result<Option<Task>, StoreError>;

    /// Admin: retry a failed or cancelled task (reset status to pending, clear iteration count).
    async fn retry_task(&self, task_id: Uuid) -> Result<(), StoreError>;
}

/// Store trait for persistent volume management.
#[async_trait]
pub trait VolumeStore: Send + Sync {
    /// Create a new volume for the given user with the specified size.
    async fn create_volume(&self, user_id: Uuid, size_bytes: i64) -> Result<Volume, StoreError>;

    /// Get a volume by ID. Returns None if not found.
    async fn get_volume(&self, volume_id: Uuid) -> Result<Option<Volume>, StoreError>;

    /// List all volumes for a user, ordered by created_at DESC.
    async fn list_volumes_for_user(&self, user_id: Uuid) -> Result<Vec<Volume>, StoreError>;

    /// Atomically attach a volume to a session.
    /// Returns Conflict if the volume is already attached to a session.
    async fn attach_volume(&self, volume_id: Uuid, session_id: Uuid) -> Result<Volume, StoreError>;

    /// Detach a volume from its current session (sets session_id = NULL).
    async fn detach_volume(&self, volume_id: Uuid) -> Result<(), StoreError>;

    /// Set the worker_id field for a volume (None to clear).
    async fn set_worker(&self, volume_id: Uuid, worker_id: Option<&str>) -> Result<(), StoreError>;

    /// Delete a volume. Only succeeds if the volume is not currently attached.
    /// Returns Conflict if attached, VolumeNotFound if not found.
    async fn delete_volume(&self, volume_id: Uuid, user_id: Uuid) -> Result<(), StoreError>;

    /// Find the volume attached to a session. Returns None if no volume is attached.
    async fn find_volume_for_session(&self, session_id: Uuid) -> Result<Option<Volume>, StoreError>;

    /// Update the last_accessed timestamp on a volume (called each turn).
    /// Only updates last_accessed; does NOT change updated_at.
    async fn touch_volume(&self, volume_id: Uuid) -> Result<(), StoreError>;
}

/// Store trait for artifact lifecycle management (object storage + Postgres metadata).
#[async_trait]
pub trait ArtifactStore: Send + Sync {
    /// Write artifact bytes to object storage via the provided opendal Operator,
    /// then insert metadata into Postgres. This is the full lifecycle "store.put()"
    /// per CONTEXT.md locked decision.
    ///
    /// Ordering: object storage write first, then Postgres insert.
    /// On Postgres failure after successful object storage write, attempts best-effort
    /// deletion of the object storage entry before returning the error.
    async fn put_artifact(
        &self,
        operator: &Operator,
        session_id: Uuid,
        user_id: Uuid,
        object_key: &str,
        data: Bytes,
        content_type: Option<&str>,
    ) -> Result<Artifact, StoreError>;

    /// List artifact metadata for a session, scoped by user_id.
    async fn list_artifacts_for_session(&self, session_id: Uuid, user_id: Uuid) -> Result<Vec<Artifact>, StoreError>;

    /// Get a single artifact's metadata by session, user, and key.
    async fn get_artifact(&self, session_id: Uuid, user_id: Uuid, object_key: &str) -> Result<Option<Artifact>, StoreError>;

    /// Delete an artifact: removes from object storage (via operator) then deletes Postgres row.
    async fn delete_artifact(
        &self,
        operator: &Operator,
        session_id: Uuid,
        user_id: Uuid,
        object_key: &str,
    ) -> Result<(), StoreError>;

    /// List artifact metadata for a user across all sessions. Cursor-based pagination.
    ///
    /// Returns up to `limit` artifacts sorted by (created_at DESC, id DESC).
    /// The cursor is the `id` (UUID string) of the last artifact in the previous page.
    /// Returns (artifacts, next_cursor) where next_cursor is Some if more pages exist.
    async fn list_artifacts_for_user(
        &self,
        user_id: Uuid,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Artifact>, Option<String>), StoreError>;

    /// Admin: list all artifacts across all users. Cursor-based pagination.
    async fn admin_list_artifacts(
        &self,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Artifact>, Option<String>), StoreError>;

    /// Delete all artifacts where expires_at < now(). For each artifact,
    /// deletes from object storage first (via operator), then deletes the
    /// Postgres row. Returns count of deleted artifacts.
    /// Skips individual artifacts that fail deletion (logs warning, continues).
    async fn delete_expired_artifacts(
        &self,
        operator: &Operator,
    ) -> Result<u64, StoreError>;
}

/// Store trait for long-running deployment VM lifecycle management.
#[async_trait]
pub trait DeploymentStore: Send + Sync {
    /// Create a new deployment record in 'starting' status.
    async fn create_deployment(&self, new: NewDeployment) -> Result<Deployment, StoreError>;

    /// Get a deployment by ID scoped to user_id. Returns None if not found or wrong user.
    async fn get_deployment(&self, id: Uuid, user_id: Uuid) -> Result<Option<Deployment>, StoreError>;

    /// List all deployments for a user ordered by created_at DESC.
    async fn list_deployments(&self, user_id: Uuid) -> Result<Vec<Deployment>, StoreError>;

    /// Update the status of a deployment. Sets stopped_at for 'stopped' and 'orphaned'.
    async fn update_deployment_status(&self, id: Uuid, status: &str) -> Result<(), StoreError>;

    /// Record the worker assignment, guest IP, and PID after a VM starts successfully.
    async fn update_deployment_worker(
        &self,
        id: Uuid,
        worker_id: &str,
        guest_ip: &str,
        pid: i32,
    ) -> Result<(), StoreError>;

    /// Atomically increment probe_failures. Returns the new count.
    async fn increment_probe_failures(&self, id: Uuid) -> Result<i32, StoreError>;

    /// Reset probe_failures to zero (called when a probe succeeds after failures).
    async fn reset_probe_failures(&self, id: Uuid) -> Result<(), StoreError>;

    /// List all deployments on a worker that are not stopped or orphaned.
    async fn list_active_on_worker(&self, worker_id: &str) -> Result<Vec<Deployment>, StoreError>;

    /// Mark all non-terminal deployments on a worker as 'orphaned'.
    /// Returns the number of rows updated.
    async fn mark_orphaned_for_worker(&self, worker_id: &str) -> Result<u64, StoreError>;

    /// Get a deployment by ID without user scoping (used by the reverse proxy,
    /// which receives unauthenticated public traffic).
    async fn get_deployment_public(&self, id: Uuid) -> Result<Option<Deployment>, StoreError>;

    /// List distinct worker_ids that have active (non-stopped, non-orphaned) deployments.
    /// Used during startup reconciliation to find workers whose deployments may be orphaned.
    async fn list_active_worker_ids(&self) -> Result<Vec<String>, StoreError>;

    /// List all deployments in 'health_checking' or 'healthy' status with a known guest_ip.
    /// Used by the health probe background task.
    async fn list_probeable_deployments(&self) -> Result<Vec<Deployment>, StoreError>;
}

/// Store trait for named shared volumes (shared between sessions via PgAdvisoryLock).
#[async_trait]
pub trait NamedVolumeStore: Send + Sync {
    /// Create a named volume for user. Returns Conflict if name already exists for this user.
    async fn create_named_volume(&self, user_id: Uuid, name: &str, size_bytes: i64) -> Result<NamedVolume, StoreError>;

    /// Get a named volume by ID. Returns None if not found.
    async fn get_named_volume(&self, id: Uuid) -> Result<Option<NamedVolume>, StoreError>;

    /// Find a named volume by owner + name. Returns None if not found.
    async fn find_named_volume_by_name(&self, user_id: Uuid, name: &str) -> Result<Option<NamedVolume>, StoreError>;

    /// List all named volumes for a user, ordered by created_at DESC.
    async fn list_named_volumes_for_user(&self, user_id: Uuid) -> Result<Vec<NamedVolume>, StoreError>;

    /// Set the worker_id for a named volume.
    async fn set_named_volume_worker(&self, id: Uuid, worker_id: Option<&str>) -> Result<(), StoreError>;

    /// Delete a named volume. Only the owner may delete. Returns NamedVolumeNotFound if absent.
    async fn delete_named_volume(&self, id: Uuid, user_id: Uuid) -> Result<(), StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // This function exists only to verify trait object safety at compile time.
    fn _assert_object_safe(_: Arc<dyn SessionStore>) {}
    fn _assert_user_store_object_safe(_: Arc<dyn UserStore>) {}
    fn _assert_storage_store_object_safe(_: Arc<dyn StorageStore>) {}
    fn _assert_task_store_object_safe(_: Arc<dyn TaskStore>) {}
    fn _assert_volume_store_object_safe(_: Arc<dyn VolumeStore>) {}
    fn _assert_artifact_store_object_safe(_: Arc<dyn ArtifactStore>) {}
    fn _assert_deployment_store_object_safe(_: Arc<dyn DeploymentStore>) {}
    fn _assert_named_volume_store_object_safe(_: Arc<dyn NamedVolumeStore>) {}
}
