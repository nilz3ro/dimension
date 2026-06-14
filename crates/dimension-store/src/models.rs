use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sqlx_core::{
    decode::Decode,
    encode::{Encode, IsNull},
    error::BoxDynError,
    from_row::FromRow,
    row::Row,
    types::Type,
};
use sqlx_postgres::{PgArgumentBuffer, PgTypeInfo, PgValueRef, Postgres};

// ── User identity models ─────────────────────────────────────────────────────

/// User role. Stored as lowercase text in the database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    User,
    Admin,
}

impl std::fmt::Display for UserRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserRole::User => write!(f, "user"),
            UserRole::Admin => write!(f, "admin"),
        }
    }
}

impl std::str::FromStr for UserRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(UserRole::User),
            "admin" => Ok(UserRole::Admin),
            other => Err(format!("unknown user role: {other}")),
        }
    }
}

impl Type<Postgres> for UserRole {
    fn type_info() -> PgTypeInfo {
        <String as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <String as Type<Postgres>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Postgres> for UserRole {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        let s = self.to_string();
        <String as Encode<'q, Postgres>>::encode(s, buf)
    }
}

impl<'r> Decode<'r, Postgres> for UserRole {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as Decode<'r, Postgres>>::decode(value)?;
        s.parse().map_err(|e: String| e.into())
    }
}

/// A registered user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub name: String,
    pub role: UserRole,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub quota_max_sessions: Option<i32>,
    pub quota_max_bundles: Option<i32>,
    pub quota_max_concurrent_vms: Option<i32>,
}

impl<'r> FromRow<'r, sqlx_postgres::PgRow> for User {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(User {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            role: row.try_get("role")?,
            created_at: row.try_get("created_at")?,
            deleted_at: row.try_get("deleted_at")?,
            quota_max_sessions: row.try_get("quota_max_sessions")?,
            quota_max_bundles: row.try_get("quota_max_bundles")?,
            quota_max_concurrent_vms: row.try_get("quota_max_concurrent_vms")?,
        })
    }
}

/// An API key record (never contains the plaintext key).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub key_hash: String,
    pub key_prefix: String,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl<'r> FromRow<'r, sqlx_postgres::PgRow> for ApiKey {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(ApiKey {
            id: row.try_get("id")?,
            user_id: row.try_get("user_id")?,
            key_hash: row.try_get("key_hash")?,
            key_prefix: row.try_get("key_prefix")?,
            label: row.try_get("label")?,
            created_at: row.try_get("created_at")?,
            revoked_at: row.try_get("revoked_at")?,
        })
    }
}

/// Authenticated user identity injected into request extensions by auth middleware.
/// This is the per-request identity, NOT the full User record.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub user_id: Uuid,
    pub name: String,
    pub role: UserRole,
}

/// Role of a message participant. Stored as lowercase text in the database.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageRole::User => write!(f, "user"),
            MessageRole::Assistant => write!(f, "assistant"),
            MessageRole::System => write!(f, "system"),
        }
    }
}

impl std::str::FromStr for MessageRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(MessageRole::User),
            "assistant" => Ok(MessageRole::Assistant),
            "system" => Ok(MessageRole::System),
            other => Err(format!("unknown message role: {other}")),
        }
    }
}

// --- sqlx Type, Encode, Decode for MessageRole (stored as TEXT in Postgres) ---

impl Type<Postgres> for MessageRole {
    fn type_info() -> PgTypeInfo {
        <String as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <String as Type<Postgres>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Postgres> for MessageRole {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        let s = self.to_string();
        <String as Encode<'q, Postgres>>::encode(s, buf)
    }
}

impl<'r> Decode<'r, Postgres> for MessageRole {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as Decode<'r, Postgres>>::decode(value)?;
        s.parse().map_err(|e: String| e.into())
    }
}

// --- Data models ---

/// An active agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    /// Nullable in Phase 1 — user association is added in Phase 2.
    pub user_id: Option<Uuid>,
    pub bundle_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Arbitrary JSON metadata for stateful conversations (HITL, state machines).
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
}

fn default_metadata() -> serde_json::Value {
    serde_json::json!({})
}

impl<'r> FromRow<'r, sqlx_postgres::PgRow> for Session {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(Session {
            id: row.try_get("id")?,
            user_id: row.try_get("user_id")?,
            bundle_id: row.try_get("bundle_id")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            metadata: row.try_get("metadata").unwrap_or_else(|_| serde_json::json!({})),
        })
    }
}

/// A single message in a session history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub session_id: Uuid,
    pub role: MessageRole,
    pub content: String,
    pub is_complete: bool,
    pub created_at: DateTime<Utc>,
}

impl<'r> FromRow<'r, sqlx_postgres::PgRow> for Message {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(Message {
            id: row.try_get("id")?,
            session_id: row.try_get("session_id")?,
            role: row.try_get("role")?,
            content: row.try_get("content")?,
            is_complete: row.try_get("is_complete")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

/// Input type for appending a message to a session.
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub role: MessageRole,
    pub content: String,
    pub is_complete: bool,
}

/// A page of message history with an optional cursor for pagination.
#[derive(Debug)]
pub struct HistoryPage {
    pub messages: Vec<Message>,
    pub next_cursor: Option<String>,
}

// ── Secrets and Tokenization models ─────────────────────────────────────────

/// Metadata for a stored secret (name only; values are never stored here).
#[derive(Debug, Clone)]
pub struct SecretMetadata {
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// A tokenization lookup record.
#[derive(Debug, Clone)]
pub struct TokenRecord {
    pub id: String,
    pub bundle_id: String,
    pub user_id: Uuid,
    pub ciphertext: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Storage usage record for a bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleStorageRecord {
    pub id: Uuid,
    pub user_id: Uuid,
    pub bundle_id: String,
    pub bytes_used: i64,
    pub quota_bytes: i64,
    pub updated_at: DateTime<Utc>,
}

impl<'r> FromRow<'r, sqlx_postgres::PgRow> for BundleStorageRecord {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(BundleStorageRecord {
            id: row.try_get("id")?,
            user_id: row.try_get("user_id")?,
            bundle_id: row.try_get("bundle_id")?,
            bytes_used: row.try_get("bytes_used")?,
            quota_bytes: row.try_get("quota_bytes")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

/// Summary returned by list_sessions -- does not include message content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: Uuid,
    pub bundle_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: i64,
}

// ── Task orchestration models ─────────────────────────────────────────────────

/// A goal-driven task with scheduling and iteration tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub user_id: Uuid,
    pub bundle_id: String,
    pub goal: String,
    pub success_criteria: Option<String>,
    pub evaluator_bundle_id: Option<String>,
    pub trigger_type: String,
    pub cron_expr: Option<String>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub max_iterations: i32,
    pub timeout_hours: i32,
    pub iteration_count: i32,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_output: Option<String>,
    pub last_eval_feedback: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input type for creating a new task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTask {
    pub user_id: Uuid,
    pub bundle_id: String,
    pub goal: String,
    pub success_criteria: Option<String>,
    pub evaluator_bundle_id: Option<String>,
    pub trigger_type: String,
    pub cron_expr: Option<String>,
    /// Pre-computed next_run_at for cron triggers (computed from cron_expr at creation time).
    /// For immediate triggers this is None (scheduler picks them up immediately).
    pub next_run_at: Option<DateTime<Utc>>,
    pub max_iterations: i32,
    pub timeout_hours: i32,
    /// Bundle IDs for fan-out targets (stored in task_targets table).
    pub target_bundle_ids: Vec<String>,
}

/// A single iteration run record for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: Uuid,
    pub task_id: Uuid,
    pub iteration: i32,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub output: Option<String>,
    pub eval_feedback: Option<String>,
    pub eval_passed: Option<bool>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Input type for recording a completed task iteration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTaskRun {
    pub task_id: Uuid,
    pub iteration: i32,
    pub output: Option<String>,
    pub eval_feedback: Option<String>,
    pub eval_passed: Option<bool>,
    pub error: Option<String>,
}

// ── Volume models ─────────────────────────────────────────────────────────────

/// A persistent volume that can be attached to a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    pub id: Uuid,
    pub user_id: Uuid,
    pub size_bytes: i64,
    pub session_id: Option<Uuid>,
    pub worker_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Tracks the last time this volume was accessed (e.g. mounted by a VM).
    /// Updated via touch_volume; independent of updated_at (which tracks record changes).
    pub last_accessed: DateTime<Utc>,
}

impl<'r> FromRow<'r, sqlx_postgres::PgRow> for Volume {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(Volume {
            id: row.try_get("id")?,
            user_id: row.try_get("user_id")?,
            size_bytes: row.try_get("size_bytes")?,
            session_id: row.try_get("session_id")?,
            worker_id: row.try_get("worker_id")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            last_accessed: row.try_get("last_accessed")?,
        })
    }
}

// ── Named Volume models ───────────────────────────────────────────────────────

/// A named shared volume that can be referenced by bundle manifests via MOUNT_SHARED.
///
/// Named volumes are not session-scoped; exclusive access is enforced via
/// PgAdvisoryLock (session handler) rather than a DB lock column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedVolume {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub size_bytes: i64,
    pub worker_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl<'r> FromRow<'r, sqlx_postgres::PgRow> for NamedVolume {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(NamedVolume {
            id: row.try_get("id")?,
            user_id: row.try_get("user_id")?,
            name: row.try_get("name")?,
            size_bytes: row.try_get("size_bytes")?,
            worker_id: row.try_get("worker_id")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

// ── Artifact models ───────────────────────────────────────────────────────────

/// An artifact stored in object storage with metadata tracked in Postgres.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: Uuid,
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub object_key: String,
    pub size_bytes: i64,
    pub content_type: Option<String>,
    pub checksum: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Optional expiry time. NULL means the artifact never expires.
    /// The GC job deletes rows where expires_at IS NOT NULL AND expires_at < now().
    pub expires_at: Option<DateTime<Utc>>,
}

impl<'r> FromRow<'r, sqlx_postgres::PgRow> for Artifact {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(Artifact {
            id: row.try_get("id")?,
            session_id: row.try_get("session_id")?,
            user_id: row.try_get("user_id")?,
            object_key: row.try_get("object_key")?,
            size_bytes: row.try_get("size_bytes")?,
            content_type: row.try_get("content_type")?,
            checksum: row.try_get("checksum")?,
            created_at: row.try_get("created_at")?,
            expires_at: row.try_get("expires_at")?,
        })
    }
}

// ── Session event models ──────────────────────────────────────────────────────

/// Event type for a session event. Stored as lowercase text in Postgres.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventType {
    Message,
    ToolCall,
    ToolResult,
    MetadataChange,
}

impl std::fmt::Display for SessionEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionEventType::Message => write!(f, "message"),
            SessionEventType::ToolCall => write!(f, "tool_call"),
            SessionEventType::ToolResult => write!(f, "tool_result"),
            SessionEventType::MetadataChange => write!(f, "metadata_change"),
        }
    }
}

impl std::str::FromStr for SessionEventType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "message" => Ok(SessionEventType::Message),
            "tool_call" => Ok(SessionEventType::ToolCall),
            "tool_result" => Ok(SessionEventType::ToolResult),
            "metadata_change" => Ok(SessionEventType::MetadataChange),
            other => Err(format!("unknown session event type: {other}")),
        }
    }
}

impl Type<Postgres> for SessionEventType {
    fn type_info() -> PgTypeInfo {
        <String as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <String as Type<Postgres>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Postgres> for SessionEventType {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        let s = self.to_string();
        <String as Encode<'q, Postgres>>::encode(s, buf)
    }
}

impl<'r> Decode<'r, Postgres> for SessionEventType {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as Decode<'r, Postgres>>::decode(value)?;
        s.parse().map_err(|e: String| e.into())
    }
}

/// A unified session event (message, tool call, or tool result).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: Uuid,
    pub session_id: Uuid,
    pub event_type: SessionEventType,
    pub role: Option<String>,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

impl<'r> FromRow<'r, sqlx_postgres::PgRow> for SessionEvent {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(SessionEvent {
            id: row.try_get("id")?,
            session_id: row.try_get("session_id")?,
            event_type: row.try_get("event_type")?,
            role: row.try_get("role")?,
            content: row.try_get("content")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

/// Input type for appending a session event.
#[derive(Debug, Clone)]
pub struct NewSessionEvent {
    pub session_id: Uuid,
    pub event_type: SessionEventType,
    pub role: Option<String>,
    pub content: String,
}

// ── Deployment models ─────────────────────────────────────────────────────────

/// A long-running deployment VM with health tracking.
///
/// Status is stored as text and maps to the deployment_status enum in Postgres.
/// This avoids requiring a custom sqlx Decode impl while keeping the DB-level
/// constraint on valid values.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Deployment {
    pub id: Uuid,
    pub user_id: Uuid,
    pub bundle_id: String,
    pub name: String,
    /// Stored as text; valid values match the deployment_status DB enum:
    /// starting, health_checking, healthy, unhealthy, stopping, stopped, orphaned
    pub status: String,
    pub worker_id: Option<String>,
    pub guest_ip: Option<String>,
    pub probe_port: i32,
    pub pid: Option<i32>,
    pub probe_failures: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub stopped_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl<'r> FromRow<'r, sqlx_postgres::PgRow> for Deployment {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(Deployment {
            id: row.try_get("id")?,
            user_id: row.try_get("user_id")?,
            bundle_id: row.try_get("bundle_id")?,
            name: row.try_get("name")?,
            status: row.try_get("status")?,
            worker_id: row.try_get("worker_id")?,
            guest_ip: row.try_get("guest_ip")?,
            probe_port: row.try_get("probe_port")?,
            pid: row.try_get("pid")?,
            probe_failures: row.try_get("probe_failures")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            stopped_at: row.try_get("stopped_at")?,
        })
    }
}

/// Input type for creating a new deployment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NewDeployment {
    pub user_id: Uuid,
    pub bundle_id: String,
    pub name: String,
    pub probe_port: i32,
}
