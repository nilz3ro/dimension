//! In-memory worker registry for tracking registered worker nodes.
//!
//! The WorkerRegistry maintains a thread-safe map of worker IDs to their state,
//! including available resources, current load, and gRPC client handles.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;
use uuid::Uuid;

use crate::worker::worker_proto::worker_service_client::WorkerServiceClient;

/// Current state of a registered worker node.
#[derive(Clone)]
pub struct WorkerState {
    /// Unique identifier for this worker.
    pub worker_id: Uuid,
    /// gRPC address used to reach this worker.
    pub grpc_addr: String,
    /// Currently available memory in megabytes.
    pub available_memory_mb: u64,
    /// Currently available vCPUs.
    pub available_vcpus: u32,
    /// Number of VMs currently running on this worker.
    pub running_vms: i32,
    /// Whether this worker is draining (not accepting new work).
    pub draining: bool,
    /// When this worker's state was last updated via health poll.
    pub last_seen: Instant,
    /// When this worker last sent a heartbeat (registration POST).
    /// Distinct from last_seen (updated by health poll).
    pub last_heartbeat: Option<Instant>,
    /// gRPC client for sending requests to this worker.
    /// Clone is cheap — the underlying channel is Arc-backed.
    pub client: WorkerServiceClient<tonic::transport::Channel>,
}

/// Thread-safe registry of all registered worker nodes.
///
/// Workers register via POST /internal/workers/register and are updated
/// via periodic health polling. Workers are removed after 3 consecutive
/// health poll failures.
pub struct WorkerRegistry {
    workers: Arc<RwLock<HashMap<Uuid, WorkerState>>>,
}

impl WorkerRegistry {
    /// Create an empty WorkerRegistry.
    pub fn new() -> Self {
        Self {
            workers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register or replace a worker in the registry.
    ///
    /// Deduplicates by `grpc_addr`: if an existing worker has the same
    /// address (e.g. after a restart with a new UUID), the old entry is
    /// evicted first.
    pub fn register(&self, state: WorkerState) {
        let mut workers = self.workers.write();
        // Evict any stale entry with the same address
        workers.retain(|id, w| *id == state.worker_id || w.grpc_addr != state.grpc_addr);
        workers.insert(state.worker_id, state);
    }

    /// Update a worker's health state after a successful health poll.
    pub fn update_health(
        &self,
        worker_id: Uuid,
        available_memory_mb: u64,
        available_vcpus: u32,
        running_vms: i32,
        draining: bool,
    ) {
        let mut workers = self.workers.write();
        if let Some(state) = workers.get_mut(&worker_id) {
            state.available_memory_mb = available_memory_mb;
            state.available_vcpus = available_vcpus;
            state.running_vms = running_vms;
            state.draining = draining;
            state.last_seen = Instant::now();
        }
    }

    /// Remove a worker from the registry (e.g., after health poll failures).
    pub fn remove(&self, worker_id: Uuid) {
        let mut workers = self.workers.write();
        workers.remove(&worker_id);
    }

    /// Return a snapshot of all registered workers.
    pub fn get_all(&self) -> Vec<WorkerState> {
        let workers = self.workers.read();
        workers.values().cloned().collect()
    }

    /// Return a single worker by ID, or None if not registered.
    pub fn get(&self, worker_id: Uuid) -> Option<WorkerState> {
        let workers = self.workers.read();
        workers.get(&worker_id).cloned()
    }

    /// Mark a worker as draining.
    pub fn mark_draining(&self, worker_id: Uuid) {
        let mut workers = self.workers.write();
        if let Some(state) = workers.get_mut(&worker_id) {
            state.draining = true;
        }
    }
}

impl Default for WorkerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
