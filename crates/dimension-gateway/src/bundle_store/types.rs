//! Types for bundle deployment job records.

use serde::Serialize;
use uuid::Uuid;

/// High-level status of a bundle deployment job.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Job has been created and is waiting to be picked up.
    Queued,
    /// Job is actively being processed.
    Running,
    /// Job completed successfully.
    Complete,
    /// Job failed at some stage.
    Failed,
}

/// Fine-grained processing stage within a job.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JobStage {
    /// Not yet started.
    Queued,
    /// Archive is being extracted and validated.
    Extracting,
    /// Bundle is being converted to a rootfs image.
    Converting,
    /// Resulting image is being registered in the bundle registry.
    Registering,
    /// All stages complete.
    Complete,
    /// Processing halted due to an error.
    Failed,
}

/// A single in-flight or completed bundle deployment job.
#[derive(Debug, Clone, Serialize)]
pub struct JobRecord {
    /// Unique job identifier.
    pub id: Uuid,
    /// High-level lifecycle status.
    pub status: JobStatus,
    /// Current processing stage.
    pub stage: JobStage,
    /// Human-readable progress description.
    pub progress_message: String,
    /// The registered bundle ID on successful completion.
    pub bundle_id: Option<i64>,
    /// Error message on failure.
    pub error: Option<String>,
    /// Unix timestamp (seconds) of job creation.
    pub created_at: i64,
    /// Unix timestamp (seconds) of last state change.
    pub updated_at: i64,
}
