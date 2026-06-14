//! Invocation observability: Clickhouse records and MinIO log uploads.
//!
//! Both clients are optional — when not configured the worker operates
//! in degraded mode with no invocation telemetry.

use chrono::{DateTime, Utc};
use opendal::{services::S3, Operator};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── InvocationRecord ───────────────────────────────────────────────────

/// Status of an invocation lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InvocationStatus {
    Running,
    Completed,
    Failed,
    Stopped,
}

impl std::fmt::Display for InvocationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

/// A single invocation record stored in Clickhouse.
///
/// Maps 1:1 to the `invocations` Clickhouse table.
#[derive(Debug, Clone, clickhouse::Row, Serialize, Deserialize)]
pub struct InvocationRecord {
    #[serde(with = "clickhouse::serde::uuid")]
    pub invocation_id: Uuid,
    pub user_id: String,
    pub bundle_id: String,
    pub worker_id: String,
    pub mode: String,
    pub status: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub log_url: String,
    pub created_at: i64,
    pub completed_at: i64,
}

/// A single outbound event stored in Clickhouse.
///
/// Maps 1:1 to the `run_events` Clickhouse table. Sources:
///   - Bundle code calling `dimension.send({...})` (kind = "bundle").
///   - Agent forwarding child stdout/stderr (kind = "stdout-final" | "stderr").
///   - Worker emitting lifecycle events (kind = "state").
#[derive(Debug, Clone, clickhouse::Row, Serialize, Deserialize)]
pub struct RunEventRecord {
    #[serde(with = "clickhouse::serde::uuid")]
    pub run_id: Uuid,
    pub seq: u64,
    pub ts: i64,
    pub kind: String,
    pub content_type: String,
    pub body: String,
    pub attrs: Vec<(String, String)>,
    pub worker_id: String,
    pub bundle_id: String,
    pub user_id: String,
}

// ── ClickhouseClient ───────────────────────────────────────────────────

/// Thin wrapper around the `clickhouse::Client` for invocation telemetry.
#[derive(Clone)]
pub struct ClickhouseClient {
    client: clickhouse::Client,
    database: String,
}

impl ClickhouseClient {
    /// Create a new client from a Clickhouse HTTP URL and database name.
    pub fn new(url: &str, database: &str) -> Self {
        let client = clickhouse::Client::default()
            .with_url(url)
            .with_database(database);
        Self {
            client,
            database: database.to_string(),
        }
    }

    /// Ensure the `invocations` table exists (CREATE TABLE IF NOT EXISTS).
    ///
    /// Should be called once at startup.
    pub async fn ensure_table(&self) -> Result<(), clickhouse::error::Error> {
        let ddl = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {db}.invocations (
                invocation_id UUID,
                user_id String,
                bundle_id String,
                worker_id String,
                mode String,
                status String,
                exit_code Int32,
                duration_ms UInt64,
                log_url String,
                created_at DateTime64(3),
                completed_at DateTime64(3)
            ) ENGINE = MergeTree()
            ORDER BY (created_at, invocation_id)
            "#,
            db = self.database
        );

        self.client.query(&ddl).execute().await?;
        Ok(())
    }

    /// Insert a single invocation record.
    pub async fn insert_invocation_record(
        &self,
        record: &InvocationRecord,
    ) -> Result<(), clickhouse::error::Error> {
        let mut insert = self.client.insert::<InvocationRecord>("invocations").await?;
        insert.write(record).await?;
        insert.end().await?;
        Ok(())
    }

    /// Ensure the `run_events` table exists.
    pub async fn ensure_run_events_table(&self) -> Result<(), clickhouse::error::Error> {
        let ddl = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {db}.run_events (
                run_id UUID,
                seq UInt64,
                ts DateTime64(3),
                kind LowCardinality(String),
                content_type LowCardinality(String),
                body String,
                attrs Map(String, String),
                worker_id LowCardinality(String),
                bundle_id LowCardinality(String),
                user_id String
            ) ENGINE = MergeTree()
            PARTITION BY toYYYYMMDD(ts)
            ORDER BY (run_id, seq)
            TTL toDateTime(ts) + INTERVAL 30 DAY
            "#,
            db = self.database
        );
        self.client.query(&ddl).execute().await?;
        Ok(())
    }

    /// Insert a batch of run event rows in a single Clickhouse INSERT.
    pub async fn insert_run_events(
        &self,
        records: &[RunEventRecord],
    ) -> Result<(), clickhouse::error::Error> {
        if records.is_empty() {
            return Ok(());
        }
        let mut insert = self.client.insert::<RunEventRecord>("run_events").await?;
        for rec in records {
            insert.write(rec).await?;
        }
        insert.end().await?;
        Ok(())
    }

}

// ── LogUploader ────────────────────────────────────────────────────────

/// Uploads invocation stdout/stderr logs to MinIO (S3-compatible storage).
///
/// Follows the same opendal S3 pattern as the gateway's `MinioClient`.
#[derive(Clone)]
pub struct LogUploader {
    operator: Operator,
}

impl LogUploader {
    /// Create a new LogUploader.
    ///
    /// Returns `None` if access_key or secret_key are not provided
    /// (degraded mode — same semantics as gateway MinioClient::connect).
    pub fn new(
        endpoint: &str,
        bucket: &str,
        access_key: Option<&str>,
        secret_key: Option<&str>,
    ) -> Option<Self> {
        let access_key = access_key?;
        let secret_key = secret_key?;

        let builder = S3::default()
            .endpoint(endpoint)
            .region("auto")
            .bucket(bucket)
            .access_key_id(access_key)
            .secret_access_key(secret_key)
            .root("/");

        let operator = Operator::new(builder).ok()?.finish();
        Some(Self { operator })
    }

    /// Upload log bytes to `invocations/{invocation_id}/output.log`.
    pub async fn upload_log(
        &self,
        invocation_id: &Uuid,
        data: Vec<u8>,
    ) -> Result<(), opendal::Error> {
        let path = format!("invocations/{}/output.log", invocation_id);
        self.operator.write(&path, data).await?;
        Ok(())
    }

    /// Build the canonical log URL for an invocation.
    pub fn log_url(bucket: &str, invocation_id: &Uuid) -> String {
        format!(
            "s3://{}/invocations/{}/output.log",
            bucket, invocation_id
        )
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Convert a `chrono::DateTime<Utc>` to the millisecond-epoch i64
/// expected by Clickhouse DateTime64(3).
pub fn datetime_to_epoch_ms(dt: DateTime<Utc>) -> i64 {
    dt.timestamp_millis()
}

/// Return the current UTC time as millisecond-epoch i64.
pub fn now_epoch_ms() -> i64 {
    datetime_to_epoch_ms(Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invocation_status_display() {
        assert_eq!(InvocationStatus::Running.to_string(), "running");
        assert_eq!(InvocationStatus::Completed.to_string(), "completed");
        assert_eq!(InvocationStatus::Failed.to_string(), "failed");
        assert_eq!(InvocationStatus::Stopped.to_string(), "stopped");
    }

    #[test]
    fn test_log_url_format() {
        let id = Uuid::parse_str("12345678-1234-1234-1234-123456789abc").unwrap();
        let url = LogUploader::log_url("dimension-logs", &id);
        assert_eq!(
            url,
            "s3://dimension-logs/invocations/12345678-1234-1234-1234-123456789abc/output.log"
        );
    }

    #[test]
    fn test_log_uploader_none_without_keys() {
        // Without access/secret key, constructor returns None (degraded mode).
        let uploader = LogUploader::new("http://localhost:9000", "bucket", None, None);
        assert!(uploader.is_none());
    }

    #[test]
    fn test_log_uploader_some_with_keys() {
        let uploader = LogUploader::new(
            "http://localhost:9000",
            "bucket",
            Some("minioadmin"),
            Some("minioadmin"),
        );
        assert!(uploader.is_some());
    }

    #[test]
    fn test_now_epoch_ms_is_reasonable() {
        let ms = now_epoch_ms();
        // Should be after 2020-01-01 and before 2100-01-01.
        assert!(ms > 1_577_836_800_000);
        assert!(ms < 4_102_444_800_000);
    }

    #[test]
    fn test_clickhouse_client_construction() {
        // Verify client can be constructed without panicking.
        let _client = ClickhouseClient::new("http://localhost:8123", "dimension");
    }

    #[test]
    fn test_invocation_record_serialization() {
        let record = InvocationRecord {
            invocation_id: Uuid::new_v4(),
            user_id: "user-1".to_string(),
            bundle_id: "bundle-1".to_string(),
            worker_id: "worker-1".to_string(),
            mode: "sync".to_string(),
            status: "completed".to_string(),
            exit_code: 0,
            duration_ms: 1500,
            log_url: "s3://dimension-logs/invocations/test/output.log".to_string(),
            created_at: now_epoch_ms(),
            completed_at: now_epoch_ms(),
        };

        // Round-trip through JSON.
        let json = serde_json::to_string(&record).unwrap();
        let deserialized: InvocationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.user_id, "user-1");
        assert_eq!(deserialized.status, "completed");
        assert_eq!(deserialized.exit_code, 0);
        assert_eq!(deserialized.duration_ms, 1500);
    }
}
