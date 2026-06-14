//! In-memory job store for bundle deployment lifecycle tracking.

pub mod types;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use uuid::Uuid;

use types::{JobRecord, JobStage, JobStatus};

/// Thread-safe in-memory store for bundle deployment job state.
///
/// Cheap to clone — the inner map is behind an `Arc`.
#[derive(Clone)]
pub struct BundleJobStore {
    inner: Arc<Mutex<HashMap<Uuid, JobRecord>>>,
}

impl BundleJobStore {
    /// Create a new, empty job store.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Insert a new job in Queued state.
    pub fn create(&self, id: Uuid) {
        let now = Utc::now().timestamp();
        let record = JobRecord {
            id,
            status: JobStatus::Queued,
            stage: JobStage::Queued,
            progress_message: "Queued".into(),
            bundle_id: None,
            error: None,
            created_at: now,
            updated_at: now,
        };
        let mut map = self.inner.lock().expect("job store lock poisoned");
        map.insert(id, record);
    }

    /// Retrieve a clone of the job record, if it exists.
    pub fn get(&self, id: &Uuid) -> Option<JobRecord> {
        let map = self.inner.lock().expect("job store lock poisoned");
        map.get(id).cloned()
    }

    /// Advance the job to a new processing stage (status becomes Running).
    pub fn set_stage(&self, id: Uuid, stage: JobStage, message: impl Into<String>) {
        let now = Utc::now().timestamp();
        let mut map = self.inner.lock().expect("job store lock poisoned");
        if let Some(record) = map.get_mut(&id) {
            record.stage = stage;
            record.status = JobStatus::Running;
            record.progress_message = message.into();
            record.updated_at = now;
        }
    }

    /// Mark the job as successfully completed.
    pub fn complete(&self, id: Uuid, bundle_id: i64) {
        let now = Utc::now().timestamp();
        let mut map = self.inner.lock().expect("job store lock poisoned");
        if let Some(record) = map.get_mut(&id) {
            record.status = JobStatus::Complete;
            record.stage = JobStage::Complete;
            record.bundle_id = Some(bundle_id);
            record.progress_message = "Complete".into();
            record.updated_at = now;
        }
    }

    /// Mark the job as failed, preserving where it failed.
    pub fn fail(&self, id: Uuid, failed_stage: JobStage, error: impl Into<String>) {
        let now = Utc::now().timestamp();
        let error_msg = error.into();
        let mut map = self.inner.lock().expect("job store lock poisoned");
        if let Some(record) = map.get_mut(&id) {
            record.status = JobStatus::Failed;
            record.stage = JobStage::Failed;
            record.error = Some(error_msg.clone());
            record.progress_message =
                format!("Failed at {:?}: {}", failed_stage, error_msg);
            record.updated_at = now;
        }
    }

    /// Remove completed or failed jobs whose `updated_at` is older than the given threshold.
    ///
    /// Running and recently-updated jobs are never removed.
    pub fn reap_stale(&self, older_than_secs: i64) {
        let cutoff = Utc::now().timestamp() - older_than_secs;
        let mut map = self.inner.lock().expect("job store lock poisoned");
        map.retain(|_, record| {
            let is_terminal =
                record.status == JobStatus::Complete || record.status == JobStatus::Failed;
            // Keep if: not terminal, or updated recently enough
            !is_terminal || record.updated_at >= cutoff
        });
    }
}

impl Default for BundleJobStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> BundleJobStore {
        BundleJobStore::new()
    }

    #[test]
    fn create_and_get_returns_queued() {
        let store = make_store();
        let id = Uuid::new_v4();
        store.create(id);

        let record = store.get(&id).expect("job should exist");
        assert_eq!(record.id, id);
        assert_eq!(record.status, JobStatus::Queued);
        assert_eq!(record.stage, JobStage::Queued);
        assert!(record.created_at > 0);
    }

    #[test]
    fn set_stage_transitions_to_running() {
        let store = make_store();
        let id = Uuid::new_v4();
        store.create(id);
        store.set_stage(id, JobStage::Extracting, "Extracting archive...");

        let record = store.get(&id).expect("job should exist");
        assert_eq!(record.status, JobStatus::Running);
        assert_eq!(record.stage, JobStage::Extracting);
        assert_eq!(record.progress_message, "Extracting archive...");
    }

    #[test]
    fn complete_sets_complete_status_and_bundle_id() {
        let store = make_store();
        let id = Uuid::new_v4();
        store.create(id);
        store.set_stage(id, JobStage::Converting, "Converting...");
        store.complete(id, 42);

        let record = store.get(&id).expect("job should exist");
        assert_eq!(record.status, JobStatus::Complete);
        assert_eq!(record.stage, JobStage::Complete);
        assert_eq!(record.bundle_id, Some(42));
    }

    #[test]
    fn fail_sets_failed_status_with_error_message() {
        let store = make_store();
        let id = Uuid::new_v4();
        store.create(id);
        store.set_stage(id, JobStage::Extracting, "Extracting...");
        store.fail(id, JobStage::Extracting, "path traversal detected");

        let record = store.get(&id).expect("job should exist");
        assert_eq!(record.status, JobStatus::Failed);
        assert_eq!(record.stage, JobStage::Failed);
        let error = record.error.expect("error field should be set");
        assert!(error.contains("path traversal"));
        // failed_stage info preserved in progress_message
        assert!(record.progress_message.contains("Extracting"));
    }

    #[test]
    fn get_unknown_id_returns_none() {
        let store = make_store();
        let unknown = Uuid::new_v4();
        assert!(store.get(&unknown).is_none());
    }

    #[test]
    fn reap_stale_removes_old_terminal_keeps_running_and_recent() {
        let store = make_store();

        // Job 1: complete but "old" — we simulate by inserting directly with a past timestamp
        let old_complete = Uuid::new_v4();
        {
            let mut map = store.inner.lock().unwrap();
            map.insert(
                old_complete,
                JobRecord {
                    id: old_complete,
                    status: JobStatus::Complete,
                    stage: JobStage::Complete,
                    progress_message: "done".into(),
                    bundle_id: Some(1),
                    error: None,
                    created_at: 1000,
                    updated_at: 1000, // very old epoch
                },
            );
        }

        // Job 2: failed but "old"
        let old_failed = Uuid::new_v4();
        {
            let mut map = store.inner.lock().unwrap();
            map.insert(
                old_failed,
                JobRecord {
                    id: old_failed,
                    status: JobStatus::Failed,
                    stage: JobStage::Failed,
                    progress_message: "err".into(),
                    bundle_id: None,
                    error: Some("boom".into()),
                    created_at: 1000,
                    updated_at: 1000,
                },
            );
        }

        // Job 3: running (should be kept)
        let running = Uuid::new_v4();
        store.create(running);
        store.set_stage(running, JobStage::Converting, "in progress");

        // Job 4: recently completed (should be kept if threshold is large)
        let recent_complete = Uuid::new_v4();
        store.create(recent_complete);
        store.complete(recent_complete, 99);

        // Reap jobs older than 60 seconds — old_complete/old_failed have timestamp=1000
        // which is far in the past; running and recent_complete are near now.
        store.reap_stale(60);

        assert!(store.get(&old_complete).is_none(), "old complete should be reaped");
        assert!(store.get(&old_failed).is_none(), "old failed should be reaped");
        assert!(store.get(&running).is_some(), "running job must not be reaped");
        assert!(
            store.get(&recent_complete).is_some(),
            "recently completed job must not be reaped"
        );
    }

    #[test]
    fn reap_does_not_remove_jobs_within_threshold() {
        let store = make_store();
        let id = Uuid::new_v4();
        store.create(id);
        store.complete(id, 1);

        // Reap with a very large threshold — this job was just created, so it's within range
        store.reap_stale(3600);
        assert!(store.get(&id).is_some(), "job within threshold must be kept");
    }
}
